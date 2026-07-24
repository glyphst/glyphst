use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui::{
    AbsoluteLength, AnyElement, App, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase,
    Div, Element, Entity, FocusHandle, Focusable, FontWeight, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, InteractiveElement, IntoElement, KeyContext,
    LayoutId, MouseButton, MouseDownEvent, MouseEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    Rems, ScrollHandle, SharedString, Stateful, StatefulInteractiveElement, StrikethroughStyle,
    Style, StyledText, TextAlign, TextLayout, TextRun, TextStyle, TextStyleRefinement,
    UnderlineStyle, Window, WrappedLineLayout, div, fill, point, prelude::*, px, rems, rgb, rgba,
};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use smallvec::SmallVec;

use crate::{
    icons::{IconName, toolbar_icon_button},
    theme::Theme,
};
use glyphst_actions::markdown::Copy;
use glyphst_settings::localize::tr;

const MONO_FONT_FAMILY: &str = ".ZedMono";

// ---------------------------------------------------------------------------
// MarkdownStyle
// ---------------------------------------------------------------------------

pub struct MarkdownStyle {
    pub base_text_style: TextStyle,
    pub container_style: gpui::StyleRefinement,
    pub selection_background_color: Hsla,
    pub code_block_actions: bool,
}

impl MarkdownStyle {
    pub fn from_theme(theme: Theme, base_text_style: TextStyle) -> Self {
        Self {
            base_text_style,
            container_style: gpui::StyleRefinement::default(),
            selection_background_color: rgba(theme.selection_highlight).into(),
            code_block_actions: false,
        }
    }

    pub fn with_code_block_actions(mut self, enabled: bool) -> Self {
        self.code_block_actions = enabled;
        self
    }
}

#[derive(Clone, Debug)]
pub struct MarkdownContextMenuRequest {
    pub position: Point<Pixels>,
    pub selected_text: Option<SharedString>,
    pub link_url: Option<SharedString>,
}

type ContextMenuCallback = Arc<dyn Fn(MarkdownContextMenuRequest, &mut Window, &mut App) + 'static>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CodeBlockId(Range<usize>);

// ---------------------------------------------------------------------------
// Markdown entity (state holder)
// ---------------------------------------------------------------------------

pub struct Markdown {
    source: SharedString,
    selection: Selection,
    pressed_link: Option<SharedString>,
    unwrapped_code_blocks: HashSet<CodeBlockId>,
    copied_code_blocks: HashMap<CodeBlockId, u64>,
    code_block_scroll_handles: HashMap<CodeBlockId, ScrollHandle>,
    focus_handle: FocusHandle,
}

impl Markdown {
    pub fn new(source: SharedString, cx: &mut Context<Self>) -> Self {
        Self {
            source,
            selection: Selection::default(),
            pressed_link: None,
            unwrapped_code_blocks: HashSet::new(),
            copied_code_blocks: HashMap::new(),
            code_block_scroll_handles: HashMap::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    #[allow(dead_code)]
    pub fn source(&self) -> &SharedString {
        &self.source
    }

    pub fn set_source(&mut self, source: SharedString, cx: &mut Context<Self>) {
        self.source = source;
        self.selection.clamp_to_source(&self.source);
        self.pressed_link = None;
        cx.notify();
    }

    #[allow(dead_code)]
    pub fn selected_text(&self) -> Option<String> {
        if self.selection.end <= self.selection.start {
            return None;
        }
        Some(self.source[self.selection.start..self.selection.end].to_string())
    }

    fn copy(&self, text: &RenderedText, _: &mut Window, cx: &mut Context<Self>) {
        if self.selection.end <= self.selection.start {
            return;
        }
        let copied = text.text_for_range(self.selection.start..self.selection.end);
        if !copied.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(copied));
        }
    }

    fn is_code_block_unwrapped(&self, id: &CodeBlockId) -> bool {
        self.unwrapped_code_blocks.contains(id)
    }

    fn toggle_code_block_wrap(&mut self, id: CodeBlockId, cx: &mut Context<Self>) {
        if !self.unwrapped_code_blocks.remove(&id) {
            self.unwrapped_code_blocks.insert(id);
        }
        cx.notify();
    }

    fn code_block_scroll_handle(&mut self, id: &CodeBlockId) -> ScrollHandle {
        self.code_block_scroll_handles
            .entry(id.clone())
            .or_default()
            .clone()
    }

    fn retain_code_block_state(&mut self, ids: &HashSet<CodeBlockId>) {
        self.unwrapped_code_blocks.retain(|id| ids.contains(id));
        self.copied_code_blocks.retain(|id, _| ids.contains(id));
        self.code_block_scroll_handles
            .retain(|id, _| ids.contains(id));
    }
}

impl Focusable for Markdown {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// ---------------------------------------------------------------------------
// Selection state
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
enum SelectMode {
    #[default]
    Character,
    Word(Range<usize>),
    Line(Range<usize>),
    All,
}

#[derive(Debug, Default, Clone)]
struct Selection {
    start: usize,
    end: usize,
    reversed: bool,
    pending: bool,
    mode: SelectMode,
}

impl Selection {
    fn set_head(&mut self, head: usize, rendered_text: &RenderedText) {
        match &self.mode {
            SelectMode::Character => {
                if head < self.tail() {
                    if !self.reversed {
                        self.end = self.start;
                        self.reversed = true;
                    }
                    self.start = head;
                } else {
                    if self.reversed {
                        self.start = self.end;
                        self.reversed = false;
                    }
                    self.end = head;
                }
            }
            SelectMode::Word(original) | SelectMode::Line(original) => {
                let head_range = if matches!(self.mode, SelectMode::Word(_)) {
                    rendered_text.surrounding_word_range(head)
                } else {
                    rendered_text.surrounding_line_range(head)
                };
                if head < original.start {
                    self.start = head_range.start;
                    self.end = original.end;
                    self.reversed = true;
                } else if head >= original.end {
                    self.start = original.start;
                    self.end = head_range.end;
                    self.reversed = false;
                } else {
                    self.start = original.start;
                    self.end = original.end;
                    self.reversed = false;
                }
            }
            SelectMode::All => {
                self.start = 0;
                self.end = rendered_text.lines.last().map_or(0, |line| line.source_end);
                self.reversed = false;
            }
        }
    }

    fn tail(&self) -> usize {
        if self.reversed { self.end } else { self.start }
    }

    fn clamp_to_source(&mut self, source: &str) {
        self.start = clamp_to_char_boundary(source, self.start);
        self.end = clamp_to_char_boundary(source, self.end);
        if self.end < self.start {
            std::mem::swap(&mut self.start, &mut self.end);
            self.reversed = !self.reversed;
        }
        self.mode = match &self.mode {
            SelectMode::Word(range) => {
                SelectMode::Word(clamp_range_to_source(source, range.clone()))
            }
            SelectMode::Line(range) => {
                SelectMode::Line(clamp_range_to_source(source, range.clone()))
            }
            mode => mode.clone(),
        };
        self.pending = false;
    }
}

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord)]
enum CharKind {
    Whitespace,
    Punctuation,
    Word,
}

struct CharClassifier;

impl CharClassifier {
    fn kind(&self, c: char) -> CharKind {
        if c.is_alphanumeric() || c == '_' {
            CharKind::Word
        } else if c.is_whitespace() {
            CharKind::Whitespace
        } else {
            CharKind::Punctuation
        }
    }
}

// ---------------------------------------------------------------------------
// Source mapping and rendered text
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct SourceMapping {
    rendered_index: usize,
    source_index: usize,
}

#[derive(Debug, Clone)]
struct RenderedLink {
    source_range: Range<usize>,
    destination_url: SharedString,
}

struct RenderedLine {
    layout: TextLayout,
    source_mappings: Vec<SourceMapping>,
    source_end: usize,
    text_align: TextAlign,
}

impl RenderedLine {
    fn rendered_index_for_source_index(&self, source_index: usize) -> usize {
        if source_index >= self.source_end {
            return self.layout.len();
        }
        let mapping = match self
            .source_mappings
            .binary_search_by_key(&source_index, |probe| probe.source_index)
        {
            Ok(ix) => &self.source_mappings[ix],
            Err(ix) => &self.source_mappings[ix - 1],
        };
        (mapping.rendered_index + (source_index - mapping.source_index)).min(self.layout.len())
    }

    fn source_index_for_rendered_index(&self, rendered_index: usize) -> usize {
        if rendered_index >= self.layout.len() {
            return self.source_end;
        }

        let mapping = match self
            .source_mappings
            .binary_search_by_key(&rendered_index, |probe| probe.rendered_index)
        {
            Ok(ix) => &self.source_mappings[ix],
            Err(ix) => &self.source_mappings[ix - 1],
        };
        mapping.source_index + (rendered_index - mapping.rendered_index)
    }

    fn source_index_for_exclusive_rendered_end(&self, rendered_index: usize) -> usize {
        if rendered_index >= self.layout.len() {
            return self.source_end;
        }

        let ix = match self
            .source_mappings
            .binary_search_by_key(&rendered_index, |probe| probe.rendered_index)
        {
            Ok(ix) => ix,
            Err(ix) => {
                return self.source_mappings[ix - 1].source_index
                    + (rendered_index - self.source_mappings[ix - 1].rendered_index);
            }
        };

        if ix > 0 {
            let prev_mapping = &self.source_mappings[ix - 1];
            let mapping = &self.source_mappings[ix];
            let prev_segment_len = mapping.rendered_index - prev_mapping.rendered_index;
            let prev_source_end = prev_mapping.source_index + prev_segment_len;
            if prev_source_end < mapping.source_index {
                return prev_source_end;
            }
        }

        self.source_mappings[ix].source_index
    }

    fn alignment_offset_for_segment(
        &self,
        available_width: Pixels,
        segment_start_x: Pixels,
        segment_end_x: Pixels,
    ) -> Pixels {
        let segment_width = segment_end_x - segment_start_x;
        match self.text_align {
            TextAlign::Left => px(0.),
            TextAlign::Center => ((available_width - segment_width) / 2.).max(px(0.)),
            TextAlign::Right => (available_width - segment_width).max(px(0.)),
        }
    }

    fn source_index_for_position(&self, position: Point<Pixels>) -> Result<usize, usize> {
        let adjusted_position = if self.text_align == TextAlign::Left {
            position
        } else if let Some(wrapped_line) = self.layout.line_layout_for_index(0) {
            let bounds = self.layout.bounds();
            let line_height = self.layout.line_height();
            let relative_y = (position.y - bounds.top()).max(px(0.));
            let wrapped_row_ix = (relative_y / line_height) as usize;
            let boundaries = wrapped_line.wrap_boundaries();

            let segment_start_x = if wrapped_row_ix == 0 {
                px(0.)
            } else {
                boundaries
                    .get(wrapped_row_ix - 1)
                    .map(|b| {
                        wrapped_line.unwrapped_layout.runs[b.run_ix].glyphs[b.glyph_ix]
                            .position
                            .x
                    })
                    .unwrap_or(px(0.))
            };
            let segment_end_x = boundaries
                .get(wrapped_row_ix)
                .map(|b| {
                    wrapped_line.unwrapped_layout.runs[b.run_ix].glyphs[b.glyph_ix]
                        .position
                        .x
                })
                .unwrap_or(wrapped_line.unwrapped_layout.width);

            let alignment_offset = self.alignment_offset_for_segment(
                bounds.size.width,
                segment_start_x,
                segment_end_x,
            );
            point(position.x - alignment_offset, position.y)
        } else {
            position
        };

        let (line_rendered_index, out_of_bounds) =
            match self.layout.index_for_position(adjusted_position) {
                Ok(ix) => (ix, false),
                Err(ix) => (ix, true),
            };
        let source_index = self.source_index_for_rendered_index(line_rendered_index);
        if out_of_bounds {
            Err(source_index)
        } else {
            Ok(source_index)
        }
    }
}

#[derive(Clone)]
struct RenderedText {
    lines: Rc<[RenderedLine]>,
    links: Rc<[RenderedLink]>,
}

struct WrappedLineSegment {
    start: usize,
    end: usize,
    row_top: Pixels,
    layout: Arc<WrappedLineLayout>,
}

impl RenderedText {
    fn bounds_for_source_range(&self, range: Range<usize>) -> Vec<Bounds<Pixels>> {
        self.bounds_for_sorted_source_ranges([(0, range)])
            .into_iter()
            .map(|(_, bounds)| bounds)
            .collect()
    }

    fn bounds_for_sorted_source_ranges(
        &self,
        ranges: impl IntoIterator<Item = (usize, Range<usize>)>,
    ) -> Vec<(usize, Bounds<Pixels>)> {
        let ranges = ranges.into_iter().collect::<Vec<_>>();
        let mut all_bounds = Vec::new();
        let mut first_possible_range_ix = 0;

        for line in self.lines.iter() {
            let line_source_start = line.source_mappings.first().unwrap().source_index;
            while ranges
                .get(first_possible_range_ix)
                .is_some_and(|(_, range)| range.end <= line_source_start)
            {
                first_possible_range_ix += 1;
            }

            let Some((_, first_possible_range)) = ranges.get(first_possible_range_ix) else {
                break;
            };
            if first_possible_range.start >= line.source_end {
                continue;
            }

            let wrapped_line_segments = Self::wrapped_line_segments(line);
            if wrapped_line_segments.is_empty() {
                continue;
            }

            let mut range_ix = first_possible_range_ix;
            while let Some((highlight_ix, range)) = ranges.get(range_ix) {
                if range.start >= line.source_end {
                    break;
                }
                Self::push_bounds_for_line_source_range(
                    &mut all_bounds,
                    *highlight_ix,
                    line,
                    &wrapped_line_segments,
                    range.start.max(line_source_start)..range.end.min(line.source_end),
                );
                range_ix += 1;
            }
        }

        all_bounds
    }

    fn wrapped_line_segments(line: &RenderedLine) -> SmallVec<[WrappedLineSegment; 1]> {
        let layout = &line.layout;
        let line_height = layout.line_height();
        let mut row_top = layout.bounds().top();
        let mut wrapped_line_start = 0;
        let mut segments = SmallVec::new();

        for wrapped_line in layout.line_layouts() {
            let wrapped_line_end = wrapped_line_start + wrapped_line.len();
            let wrapped_line_height = wrapped_line.size(line_height).height;
            segments.push(WrappedLineSegment {
                start: wrapped_line_start,
                end: wrapped_line_end,
                row_top,
                layout: wrapped_line,
            });
            row_top += wrapped_line_height;
            wrapped_line_start = wrapped_line_end + 1;
        }

        segments
    }

    fn push_bounds_for_line_source_range(
        all_bounds: &mut Vec<(usize, Bounds<Pixels>)>,
        highlight_ix: usize,
        line: &RenderedLine,
        wrapped_line_segments: &[WrappedLineSegment],
        range: Range<usize>,
    ) {
        if range.start >= range.end {
            return;
        }

        let layout = &line.layout;
        let line_bounds = layout.bounds();
        let line_height = layout.line_height();

        let rendered_start = line.rendered_index_for_source_index(range.start);
        let rendered_end = line.rendered_index_for_source_index(range.end);

        for wrapped_line_segment in wrapped_line_segments {
            if wrapped_line_segment.start >= rendered_end {
                break;
            }
            if wrapped_line_segment.end <= rendered_start {
                continue;
            }

            let wrapped_line = &wrapped_line_segment.layout;
            let unwrapped_layout = &wrapped_line.unwrapped_layout;
            let wrapped_line_start = wrapped_line_segment.start;
            let wrapped_line_end = wrapped_line_segment.end;
            let mut row_top = wrapped_line_segment.row_top;

            let row_ends = wrapped_line
                .wrap_boundaries()
                .iter()
                .map(|wrap_boundary| {
                    let glyph =
                        &unwrapped_layout.runs[wrap_boundary.run_ix].glyphs[wrap_boundary.glyph_ix];
                    (wrapped_line_start + glyph.index, glyph.position.x)
                })
                .chain([(wrapped_line_end, unwrapped_layout.width)]);

            let mut row_start = wrapped_line_start;
            let mut row_start_x = Pixels::ZERO;

            for (row_end, row_end_x) in row_ends {
                let selection_start = rendered_start.max(row_start);
                let selection_end = rendered_end.min(row_end);

                if selection_start < selection_end {
                    let alignment_offset = line.alignment_offset_for_segment(
                        line_bounds.size.width,
                        row_start_x,
                        row_end_x,
                    );
                    let x_for_index = |index| {
                        line_bounds.left()
                            + alignment_offset
                            + unwrapped_layout.x_for_index(index - wrapped_line_start)
                            - row_start_x
                    };
                    all_bounds.push((
                        highlight_ix,
                        Bounds::from_corners(
                            point(x_for_index(selection_start), row_top),
                            point(x_for_index(selection_end), row_top + line_height),
                        ),
                    ));
                }

                row_start = row_end;
                row_start_x = row_end_x;
                row_top += line_height;
            }
        }
    }

    fn source_index_for_position(&self, position: Point<Pixels>) -> Result<usize, usize> {
        let mut lines = self.lines.iter().peekable();
        let mut fallback_line: Option<&RenderedLine> = None;

        while let Some(line) = lines.next() {
            let line_bounds = line.layout.bounds();

            if line_bounds.contains(&position) {
                return line.source_index_for_position(position);
            }

            if position.y <= line_bounds.bottom() && fallback_line.is_none() {
                fallback_line = Some(line);
            }

            if position.y > line_bounds.bottom()
                && let Some(next_line) = lines.peek()
                && position.y < next_line.layout.bounds().top()
            {
                return Err(line.source_end);
            }
        }

        if let Some(line) = fallback_line {
            return line.source_index_for_position(position);
        }

        Err(self.lines.last().map_or(0, |line| line.source_end))
    }

    fn text_for_range(&self, range: Range<usize>) -> String {
        let mut accumulator = String::new();
        for line in self.lines.iter() {
            if range.start > line.source_end {
                continue;
            }
            let Some(first_mapping) = line.source_mappings.first() else {
                continue;
            };
            let line_source_start = first_mapping.source_index;
            if range.end < line_source_start {
                break;
            }
            let text = line.layout.text();
            let start = if range.start < line_source_start {
                0
            } else {
                line.rendered_index_for_source_index(range.start)
            };
            let end = if range.end > line.source_end {
                line.rendered_index_for_source_index(line.source_end)
            } else {
                line.rendered_index_for_source_index(range.end)
            }
            .min(text.len());

            if start < end {
                accumulator.push_str(&text[start..end]);
            }
            accumulator.push('\n');
        }
        // Remove trailing newline
        accumulator.pop();
        accumulator
    }

    fn surrounding_word_range(&self, source_index: usize) -> Range<usize> {
        for line in self.lines.iter() {
            if source_index > line.source_end {
                continue;
            }
            let line_rendered_start = line.source_mappings.first().unwrap().rendered_index;
            let rendered_index_in_line =
                line.rendered_index_for_source_index(source_index) - line_rendered_start;
            let text = line.layout.text();

            let classifier = CharClassifier;
            let mut prev_chars = text[..rendered_index_in_line].chars().rev().peekable();
            let mut next_chars = text[rendered_index_in_line..].chars().peekable();

            let word_kind = std::cmp::max(
                prev_chars.peek().map(|&c| classifier.kind(c)),
                next_chars.peek().map(|&c| classifier.kind(c)),
            );

            let mut start = rendered_index_in_line;
            for c in prev_chars {
                if Some(classifier.kind(c)) == word_kind {
                    start -= c.len_utf8();
                } else {
                    break;
                }
            }

            let mut end = rendered_index_in_line;
            for c in next_chars {
                if Some(classifier.kind(c)) == word_kind {
                    end += c.len_utf8();
                } else {
                    break;
                }
            }

            return line.source_index_for_rendered_index(line_rendered_start + start)
                ..line.source_index_for_exclusive_rendered_end(line_rendered_start + end);
        }
        source_index..source_index
    }

    fn surrounding_line_range(&self, source_index: usize) -> Range<usize> {
        for line in self.lines.iter() {
            if source_index > line.source_end {
                continue;
            }
            let Some(first_mapping) = line.source_mappings.first() else {
                continue;
            };
            let line_source_start = first_mapping.source_index;
            return line_source_start..line.source_end;
        }
        source_index..source_index
    }

    fn link_for_source_index(&self, source_index: usize) -> Option<&RenderedLink> {
        self.links
            .iter()
            .find(|link| link.source_range.contains(&source_index))
    }
}

// ---------------------------------------------------------------------------
// RenderedMarkdown (build result)
// ---------------------------------------------------------------------------

pub struct RenderedMarkdown {
    element: AnyElement,
    text: RenderedText,
}

// ---------------------------------------------------------------------------
// MarkdownElement (custom GPUI Element)
// ---------------------------------------------------------------------------

pub struct MarkdownElement {
    markdown: Entity<Markdown>,
    style: MarkdownStyle,
    on_context_menu: Option<ContextMenuCallback>,
}

impl MarkdownElement {
    pub fn new(markdown: Entity<Markdown>, style: MarkdownStyle) -> Self {
        Self {
            markdown,
            style,
            on_context_menu: None,
        }
    }

    pub fn on_context_menu(
        mut self,
        callback: impl Fn(MarkdownContextMenuRequest, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_context_menu = Some(Arc::new(callback));
        self
    }

    fn on_mouse_event<T: MouseEvent>(
        &self,
        window: &mut Window,
        _cx: &mut App,
        mut f: impl FnMut(&mut Markdown, &T, DispatchPhase, &mut Window, &mut Context<Markdown>)
        + 'static,
    ) {
        window.on_mouse_event({
            let markdown = self.markdown.downgrade();
            move |event, phase, window, cx| {
                let _ = markdown.update(cx, |markdown, cx| f(markdown, event, phase, window, cx));
            }
        });
    }

    fn paint_selection(&self, rendered_text: &RenderedText, window: &mut Window, cx: &mut App) {
        let selection = self.markdown.read(cx).selection.clone();
        for bounds in rendered_text.bounds_for_source_range(selection.start..selection.end) {
            window.paint_quad(fill(bounds, self.style.selection_background_color));
        }
    }

    fn paint_mouse_listeners(
        &self,
        hitbox: &Hitbox,
        rendered_text: &RenderedText,
        window: &mut Window,
        cx: &mut App,
    ) {
        let is_hovering_link = hitbox.is_hovered(window)
            && !self.markdown.read(cx).selection.pending
            && rendered_text
                .source_index_for_position(window.mouse_position())
                .ok()
                .is_some_and(|ix| rendered_text.link_for_source_index(ix).is_some());

        if is_hovering_link {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        } else {
            window.set_cursor_style(CursorStyle::IBeam, hitbox);
        }

        if let Some(on_context_menu) = self.on_context_menu.clone() {
            self.on_mouse_event(window, cx, {
                let rendered_text = rendered_text.clone();
                let hitbox = hitbox.clone();
                move |markdown, event: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture()
                        || event.button != MouseButton::Right
                        || !hitbox.is_hovered(window)
                    {
                        return;
                    }

                    let link_url = rendered_text
                        .source_index_for_position(event.position)
                        .ok()
                        .and_then(|source_index| rendered_text.link_for_source_index(source_index))
                        .map(|link| link.destination_url.clone());
                    let selected_text = (markdown.selection.start < markdown.selection.end)
                        .then(|| {
                            rendered_text
                                .text_for_range(markdown.selection.start..markdown.selection.end)
                        })
                        .filter(|text| !text.is_empty())
                        .map(SharedString::from);

                    on_context_menu(
                        MarkdownContextMenuRequest {
                            position: event.position,
                            selected_text,
                            link_url,
                        },
                        window,
                        cx,
                    );
                }
            });
        }

        // Mouse down: start selection or click link
        self.on_mouse_event(window, cx, {
            let rendered_text = rendered_text.clone();
            let hitbox = hitbox.clone();
            move |markdown, event: &MouseDownEvent, phase, window, cx| {
                if !hitbox.is_hovered(window) {
                    if phase.capture() && event.button == MouseButton::Left {
                        markdown.selection = Selection::default();
                        cx.notify();
                    }
                    return;
                }
                if phase.bubble() && event.button != MouseButton::Right {
                    let position_result = rendered_text.source_index_for_position(event.position);
                    let source_index = match position_result {
                        Ok(ix) | Err(ix) => ix,
                    };

                    // Check for link click
                    if let Some(link) = rendered_text.link_for_source_index(source_index)
                        && event.click_count == 1
                    {
                        markdown.pressed_link = Some(link.destination_url.clone());
                        window.focus(&markdown.focus_handle, cx);
                        window.prevent_default();
                        cx.notify();
                        return;
                    }

                    let (range, mode, reversed) = match event.click_count {
                        1 if event.modifiers.shift => {
                            let tail = markdown.selection.tail();
                            let reversed = source_index < tail;
                            let range = if reversed {
                                source_index..tail
                            } else {
                                tail..source_index
                            };
                            (range, SelectMode::Character, reversed)
                        }
                        1 => {
                            let range = source_index..source_index;
                            (range, SelectMode::Character, false)
                        }
                        2 => {
                            let range = rendered_text.surrounding_word_range(source_index);
                            (range.clone(), SelectMode::Word(range), false)
                        }
                        3 => {
                            let range = rendered_text.surrounding_line_range(source_index);
                            (range.clone(), SelectMode::Line(range), false)
                        }
                        _ => {
                            let range = 0..rendered_text.lines.last().map_or(0, |l| l.source_end);
                            (range, SelectMode::All, false)
                        }
                    };
                    markdown.selection = Selection {
                        start: range.start,
                        end: range.end,
                        reversed,
                        pending: true,
                        mode,
                    };
                    window.focus(&markdown.focus_handle, cx);
                    window.prevent_default();
                    cx.notify();
                }
            }
        });

        // Mouse move: extend selection during drag
        self.on_mouse_event(window, cx, {
            let rendered_text = rendered_text.clone();
            move |markdown, event: &MouseMoveEvent, phase, _window, cx| {
                if phase.capture() {
                    return;
                }
                if markdown.selection.pending {
                    let source_index = match rendered_text.source_index_for_position(event.position)
                    {
                        Ok(ix) | Err(ix) => ix,
                    };
                    markdown.selection.set_head(source_index, &rendered_text);
                    cx.notify();
                }
            }
        });

        // Mouse up: finalize selection, open link if applicable
        self.on_mouse_event(window, cx, {
            let rendered_text = rendered_text.clone();
            move |markdown, event: &MouseUpEvent, phase, _window, cx| {
                if phase.bubble() {
                    let source_index = rendered_text.source_index_for_position(event.position).ok();
                    if let Some(pressed_url) = markdown.pressed_link.take()
                        && let Some(ix) = source_index
                        && rendered_text.link_for_source_index(ix).is_some()
                    {
                        cx.open_url(&pressed_url);
                    }
                    markdown.selection.pending = false;
                    cx.notify();
                }
            }
        });
    }
}

impl IntoElement for MarkdownElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for MarkdownElement {
    type RequestLayoutState = RenderedMarkdown;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let source = self.markdown.read(cx).source.clone();
        let theme = cx.global::<crate::settings::SettingsStore>().resolved_theme;

        let options = markdown_options();
        let mut builder = MarkdownElementBuilder::new(
            theme,
            self.style.base_text_style.clone(),
            &self.style.container_style,
            self.markdown.clone(),
            self.style.code_block_actions,
        );

        for (event, range) in Parser::new_ext(&source, options).into_offset_iter() {
            builder.process_event(event, range, cx);
        }

        let (mut rendered_markdown, code_block_ids) = builder.build();
        if self.style.code_block_actions {
            self.markdown.update(cx, |markdown, _| {
                markdown.retain_code_block_state(&code_block_ids)
            });
        }
        let child_layout_id = rendered_markdown.element.request_layout(window, cx);
        let layout_id = window.request_layout(Style::default(), [child_layout_id], cx);
        (layout_id, rendered_markdown)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        rendered_markdown: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let focus_handle = self.markdown.read(cx).focus_handle.clone();
        window.set_focus_handle(&focus_handle, cx);

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        rendered_markdown.element.prepaint(window, cx);
        hitbox
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        rendered_markdown: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut context = KeyContext::default();
        context.add("Markdown");
        window.set_key_context(context);

        // Register copy action
        window.on_action(std::any::TypeId::of::<Copy>(), {
            let entity = self.markdown.clone();
            let text = rendered_markdown.text.clone();
            move |_, phase, window, cx| {
                let text = text.clone();
                if phase == DispatchPhase::Bubble {
                    entity.update(cx, move |this, cx| this.copy(&text, window, cx))
                }
            }
        });

        self.paint_mouse_listeners(hitbox, &rendered_markdown.text, window, cx);
        rendered_markdown.element.paint(window, cx);
        self.paint_selection(&rendered_markdown.text, window, cx);
    }
}

// ---------------------------------------------------------------------------
// MarkdownElementBuilder
// ---------------------------------------------------------------------------

#[derive(Default)]
struct PendingLine {
    text: String,
    runs: Vec<TextRun>,
    source_mappings: Vec<SourceMapping>,
}

#[derive(Clone, Debug)]
struct CodeBlockDescriptor {
    id: CodeBlockId,
    content: SharedString,
}

struct ActiveCodeBlock {
    descriptor: CodeBlockDescriptor,
    content: String,
    unwrapped: bool,
    enhanced: bool,
}

enum BuilderDiv {
    Div(Div),
    Stateful(Stateful<Div>),
}

impl BuilderDiv {
    fn append_child(&mut self, child: AnyElement) {
        match self {
            Self::Div(div) => div.extend([child]),
            Self::Stateful(div) => div.extend([child]),
        }
    }

    fn into_any_element(self) -> AnyElement {
        match self {
            Self::Div(div) => div.into_any_element(),
            Self::Stateful(div) => div.into_any_element(),
        }
    }
}

struct MarkdownElementBuilder {
    theme: Theme,
    base_text_style: TextStyle,
    markdown: Entity<Markdown>,
    code_block_actions: bool,
    code_block_ids: HashSet<CodeBlockId>,
    code_block_stack: Vec<ActiveCodeBlock>,
    div_stack: Vec<BuilderDiv>,
    text_style_stack: Vec<TextStyleRefinement>,
    pending_line: PendingLine,
    rendered_lines: Vec<RenderedLine>,
    rendered_links: Vec<RenderedLink>,
    current_source_index: usize,
    list_stack: Vec<Option<u64>>,
    table_alignments: Vec<Alignment>,
    in_table_head: bool,
    table_col_count: usize,
    table_col_index: usize,
    link_start_stack: Vec<(usize, SharedString)>,
}

impl MarkdownElementBuilder {
    fn new(
        theme: Theme,
        base_text_style: TextStyle,
        container_style: &gpui::StyleRefinement,
        markdown: Entity<Markdown>,
        code_block_actions: bool,
    ) -> Self {
        let mut root_div = div().flex().flex_col().w_full();
        root_div.style().refine(container_style);
        Self {
            theme,
            base_text_style,
            markdown,
            code_block_actions,
            code_block_ids: HashSet::new(),
            code_block_stack: Vec::new(),
            div_stack: vec![BuilderDiv::Div(root_div)],
            text_style_stack: Vec::new(),
            pending_line: PendingLine::default(),
            rendered_lines: Vec::new(),
            rendered_links: Vec::new(),
            current_source_index: 0,
            list_stack: Vec::new(),
            table_alignments: Vec::new(),
            in_table_head: false,
            table_col_count: 0,
            table_col_index: 0,
            link_start_stack: Vec::new(),
        }
    }

    fn text_style(&self) -> TextStyle {
        let mut style = self.base_text_style.clone();
        for refinement in &self.text_style_stack {
            style.refine(refinement);
        }
        style
    }

    fn current_cell_text_align(&self) -> TextAlign {
        self.table_alignments
            .get(self.table_col_index)
            .copied()
            .and_then(alignment_to_text_align)
            .unwrap_or(self.base_text_style.text_align)
    }

    fn push_text_style(&mut self, refinement: TextStyleRefinement) {
        self.text_style_stack.push(refinement);
    }

    fn pop_text_style(&mut self) {
        self.text_style_stack.pop();
    }

    fn push_div(&mut self, div: Div) {
        self.flush_text();
        self.div_stack.push(BuilderDiv::Div(div));
    }

    fn push_stateful_div(&mut self, div: Stateful<Div>) {
        self.flush_text();
        self.div_stack.push(BuilderDiv::Stateful(div));
    }

    fn pop_div(&mut self) {
        self.flush_text();
        let div = self.div_stack.pop().unwrap();
        self.append_child(div.into_any_element());
    }

    fn append_child(&mut self, child: AnyElement) {
        self.div_stack.last_mut().unwrap().append_child(child);
    }

    fn push_text(&mut self, text: &str, source_range: Range<usize>) {
        if text.is_empty() {
            return;
        }
        self.pending_line.source_mappings.push(SourceMapping {
            rendered_index: self.pending_line.text.len(),
            source_index: source_range.start,
        });
        self.pending_line.text.push_str(text);
        self.current_source_index = source_range.end;
        let style = self.text_style();
        self.pending_line.runs.push(style.to_run(text.len()));
    }

    fn flush_text(&mut self) {
        let text_align = self.text_style().text_align;
        let line = std::mem::take(&mut self.pending_line);
        if line.text.is_empty() {
            return;
        }
        let styled = StyledText::new(SharedString::from(line.text)).with_runs(line.runs);
        self.rendered_lines.push(RenderedLine {
            layout: styled.layout().clone(),
            source_mappings: line.source_mappings,
            source_end: self.current_source_index,
            text_align,
        });
        self.append_child(styled.into_any_element());
    }

    fn trim_trailing_newline(&mut self) {
        if self.pending_line.text.ends_with('\n') {
            self.pending_line
                .text
                .truncate(self.pending_line.text.len() - 1);
            if let Some(last) = self.pending_line.runs.last_mut() {
                last.len -= 1;
            }
            if self.current_source_index > 0 {
                self.current_source_index -= 1;
            }
        }
    }

    fn process_event(&mut self, event: Event, range: Range<usize>, cx: &mut App) {
        match event {
            Event::Start(tag) => self.process_start(tag, range, cx),
            Event::End(tag) => self.process_end(tag, range, cx),
            Event::Text(text) => {
                if let Some(code_block) = self
                    .code_block_stack
                    .last_mut()
                    .filter(|code_block| code_block.enhanced)
                {
                    code_block.content.push_str(&text);
                }
                self.push_text(&text, range);
            }
            Event::Code(code) => self.process_code_span(&code, range),
            Event::SoftBreak => self.push_text(" ", range),
            Event::HardBreak => self.push_text("\n", range),
            Event::Rule => self.process_rule(),
            Event::TaskListMarker(checked) => {
                self.push_text(if checked { "[x] " } else { "[ ] " }, range)
            }
            _ => {}
        }
    }

    fn process_start(&mut self, tag: Tag, range: Range<usize>, cx: &mut App) {
        match tag {
            Tag::Paragraph => {
                let align = self.current_cell_text_align();
                let paragraph = align_div(div().mb_2().line_height(rems(1.3)), align);
                self.push_text_style(TextStyleRefinement {
                    text_align: Some(align),
                    ..Default::default()
                });
                self.push_div(paragraph);
            }
            Tag::Heading { level, .. } => {
                let factor = heading_scale(level);
                let align = self.current_cell_text_align();
                let heading_div = align_div(
                    div()
                        .mt_4()
                        .mb_2()
                        .line_height(rems(1.2))
                        .text_color(rgb(self.theme.text)),
                    align,
                );
                self.push_div(heading_div);
                self.push_text_style(TextStyleRefinement {
                    font_size: Some(scaled_font_size(&self.base_text_style, factor)),
                    font_weight: Some(FontWeight::BOLD),
                    color: Some(rgb(self.theme.text).into()),
                    text_align: Some(align),
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
                let descriptor = CodeBlockDescriptor {
                    id: code_block_id(&self.markdown.read(cx).source, &range),
                    content: SharedString::default(),
                };
                let unwrapped = self.code_block_actions
                    && self
                        .markdown
                        .read(cx)
                        .is_code_block_unwrapped(&descriptor.id);
                let mut code_block = div()
                    .bg(rgb(self.theme.panel_background))
                    .border_1()
                    .border_color(rgb(self.theme.border_variant))
                    .rounded(px(4.0))
                    .p_3()
                    .mb_2();
                if self.code_block_actions {
                    code_block = code_block
                        .group("markdown-code-block")
                        .relative()
                        .w_full()
                        .overflow_hidden();
                }
                self.push_div(code_block);
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
                if self.code_block_actions && unwrapped {
                    let scroll_handle = self.markdown.update(cx, |markdown, _| {
                        markdown.code_block_scroll_handle(&descriptor.id)
                    });
                    let mut scroll = div()
                        .id(format!(
                            "markdown-code-scroll-{}-{}",
                            descriptor.id.0.start, descriptor.id.0.end
                        ))
                        .flex()
                        .w_full()
                        .min_w_0()
                        .overflow_x_scroll()
                        .track_scroll(&scroll_handle);
                    scroll.style().restrict_scroll_to_axis = Some(true);
                    self.push_stateful_div(scroll);
                    self.push_div(div().flex_none().whitespace_nowrap());
                }
                self.code_block_stack.push(ActiveCodeBlock {
                    descriptor,
                    content: String::new(),
                    unwrapped,
                    enhanced: self.code_block_actions,
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
                    font_style: Some(gpui::FontStyle::Italic),
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
            Tag::Link { dest_url, .. } => {
                self.link_start_stack
                    .push((range.start, SharedString::from(dest_url.to_string())));
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
                let text_align =
                    alignment_to_text_align(alignment).unwrap_or(self.base_text_style.text_align);
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
                cell = match text_align {
                    TextAlign::Center => cell.items_center().text_center(),
                    TextAlign::Right => cell.items_end().text_right(),
                    TextAlign::Left => cell.items_start().text_left(),
                };
                self.push_text_style(TextStyleRefinement {
                    text_align: Some(text_align),
                    ..Default::default()
                });
                self.push_div(cell);
            }
            Tag::HtmlBlock => {
                self.push_div(div().mb_2());
            }
            _ => {}
        }
    }

    fn process_end(&mut self, tag: TagEnd, range: Range<usize>, cx: &mut App) {
        match tag {
            TagEnd::Paragraph => {
                self.pop_div();
                self.pop_text_style();
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
                let mut active = self
                    .code_block_stack
                    .pop()
                    .unwrap_or_else(|| ActiveCodeBlock {
                        descriptor: CodeBlockDescriptor {
                            id: CodeBlockId(range.clone()),
                            content: SharedString::default(),
                        },
                        content: String::new(),
                        unwrapped: false,
                        enhanced: false,
                    });
                if active.enhanced {
                    active.descriptor.content = active.content.into();
                    self.code_block_ids.insert(active.descriptor.id.clone());
                    if active.unwrapped {
                        self.pop_div();
                        self.pop_div();
                    }
                    let copied = self
                        .markdown
                        .read(cx)
                        .copied_code_blocks
                        .contains_key(&active.descriptor.id);
                    self.append_child(
                        render_code_block_buttons(
                            active.descriptor,
                            active.unwrapped,
                            copied,
                            self.markdown.clone(),
                            self.theme,
                        )
                        .into_any_element(),
                    );
                }
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
                if let Some((start, url)) = self.link_start_stack.pop() {
                    self.rendered_links.push(RenderedLink {
                        source_range: start..range.end,
                        destination_url: url,
                    });
                }
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
                self.pop_text_style();
                self.table_col_index += 1;
            }
            TagEnd::HtmlBlock => {
                self.pop_div();
            }
            _ => {}
        }
    }

    fn process_code_span(&mut self, code: &str, range: Range<usize>) {
        let bg = rgba((self.theme.control_background << 8) | 0x88).into();
        self.push_text_style(TextStyleRefinement {
            font_family: Some(MONO_FONT_FAMILY.into()),
            background_color: Some(bg),
            color: Some(rgb(self.theme.text).into()),
            ..Default::default()
        });
        self.push_text(code, range);
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

    fn build(mut self) -> (RenderedMarkdown, HashSet<CodeBlockId>) {
        self.flush_text();
        let element = self.div_stack.pop().unwrap().into_any_element();
        (
            RenderedMarkdown {
                element,
                text: RenderedText {
                    lines: self.rendered_lines.into(),
                    links: self.rendered_links.into(),
                },
            },
            self.code_block_ids,
        )
    }
}

fn render_code_block_buttons(
    descriptor: CodeBlockDescriptor,
    unwrapped: bool,
    copied: bool,
    markdown: Entity<Markdown>,
    theme: Theme,
) -> Div {
    let wrap_id = descriptor.id.clone();
    let copy_id = descriptor.id.clone();
    let code = descriptor.content.clone();
    let wrap_markdown = markdown.clone();
    let wrap_tooltip = if unwrapped {
        tr("markdown-wrap-code")
    } else {
        tr("markdown-unwrap-code")
    };

    let wrap_button = toolbar_icon_button(
        format!(
            "markdown-code-wrap-{}-{}",
            descriptor.id.0.start, descriptor.id.0.end
        ),
        if unwrapped {
            IconName::TextWrap
        } else {
            IconName::TextUnwrap
        },
    )
    .tooltip(wrap_tooltip)
    .on_click(move |_, _, cx| {
        wrap_markdown.update(cx, |markdown, cx| {
            markdown.toggle_code_block_wrap(wrap_id.clone(), cx);
        });
    });

    let copy_markdown = markdown;
    let copy_button = toolbar_icon_button(
        format!(
            "markdown-code-copy-{}-{}",
            descriptor.id.0.start, descriptor.id.0.end
        ),
        if copied {
            IconName::Check
        } else {
            IconName::Copy
        },
    )
    .when(copied, |this| this.text_color(rgb(theme.success)))
    .tooltip(if copied {
        tr("ai-copied")
    } else {
        tr("markdown-copy-code")
    })
    .on_click(move |_, _, cx| {
        let copy_id = copy_id.clone();
        let code = code.clone();
        copy_markdown.update(cx, |markdown, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
            let generation = markdown
                .copied_code_blocks
                .entry(copy_id.clone())
                .and_modify(|generation| *generation = generation.wrapping_add(1))
                .or_insert(0);
            let generation = *generation;
            cx.notify();

            cx.spawn(async move |markdown, cx| {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let _ = markdown.update(cx, |markdown, cx| {
                    if markdown.copied_code_blocks.get(&copy_id) == Some(&generation) {
                        markdown.copied_code_blocks.remove(&copy_id);
                        cx.notify();
                    }
                });
            })
            .detach();
        });
    });

    div()
        .absolute()
        .top_1()
        .right_1()
        .flex()
        .gap_0p5()
        .p_0p5()
        .rounded(px(4.0))
        .bg(rgb(theme.panel_background))
        .opacity(0.0)
        .group_hover("markdown-code-block", |style| style.opacity(1.0))
        .child(wrap_button)
        .child(copy_button)
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        .union(Options::ENABLE_STRIKETHROUGH)
        .union(Options::ENABLE_TASKLISTS)
}

fn code_block_id(source: &str, range: &Range<usize>) -> CodeBlockId {
    let local_content_range = extract_code_block_content_range(&source[range.clone()]);
    CodeBlockId(range.start..range.start + local_content_range.start)
}

fn extract_code_block_content_range(text: &str) -> Range<usize> {
    let Some(first_line_end) = text.find('\n') else {
        return if fenced_marker(text).is_some() {
            text.len()..text.len()
        } else {
            0..text.len()
        };
    };
    let first_line = &text[..first_line_end];
    let Some((fence_char, fence_len)) = fenced_marker(first_line) else {
        return 0..text.len();
    };

    let content_start = first_line_end + 1;
    let without_trailing_newline = text.strip_suffix('\n').unwrap_or(text);
    let (last_line_start, is_closing_fence) = if without_trailing_newline.len() < content_start {
        (content_start, false)
    } else {
        let last_line_start = without_trailing_newline
            .rfind('\n')
            .map_or(content_start, |index| index + 1);
        let last_line = &without_trailing_newline[last_line_start..];
        (
            last_line_start,
            closing_fence_matches(last_line, fence_char, fence_len),
        )
    };
    let content_end = if is_closing_fence {
        last_line_start
    } else {
        text.len()
    };

    content_start.min(content_end)..content_end
}

fn fenced_marker(line: &str) -> Option<(char, usize)> {
    let marker = line.trim_start_matches(' ');
    if line.len() - marker.len() > 3 {
        return None;
    }
    let fence_char = marker.chars().next()?;
    if fence_char != '`' && fence_char != '~' {
        return None;
    }
    let fence_len = marker
        .chars()
        .take_while(|char| *char == fence_char)
        .count();
    (fence_len >= 3).then_some((fence_char, fence_len))
}

fn closing_fence_matches(line: &str, fence_char: char, minimum_len: usize) -> bool {
    let marker = line.trim_start_matches(' ');
    if line.len() - marker.len() > 3 {
        return false;
    }
    let fence_len = marker
        .chars()
        .take_while(|char| *char == fence_char)
        .count();
    fence_len >= minimum_len && marker[fence_len..].trim().is_empty()
}

fn clamp_to_char_boundary(source: &str, index: usize) -> usize {
    let mut index = index.min(source.len());
    while !source.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn clamp_range_to_source(source: &str, range: Range<usize>) -> Range<usize> {
    let start = clamp_to_char_boundary(source, range.start);
    let end = clamp_to_char_boundary(source, range.end);
    start.min(end)..start.max(end)
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

fn alignment_to_text_align(alignment: Alignment) -> Option<TextAlign> {
    match alignment {
        Alignment::Left => Some(TextAlign::Left),
        Alignment::Center => Some(TextAlign::Center),
        Alignment::Right => Some(TextAlign::Right),
        Alignment::None => None,
    }
}

fn align_div(div: Div, align: TextAlign) -> Div {
    match align {
        TextAlign::Center => div.text_center(),
        TextAlign::Right => div.text_right(),
        TextAlign::Left => div.text_left(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed_code_blocks(source: &str) -> Vec<(CodeBlockId, String)> {
        let mut active = None;
        let mut blocks = Vec::new();
        for (event, range) in Parser::new_ext(source, markdown_options()).into_offset_iter() {
            match event {
                Event::Start(Tag::CodeBlock(_)) => {
                    active = Some((code_block_id(source, &range), String::new()));
                }
                Event::Text(text) => {
                    if let Some((_, content)) = active.as_mut() {
                        content.push_str(&text);
                    }
                }
                Event::End(TagEnd::CodeBlock) => {
                    if let Some(block) = active.take() {
                        blocks.push(block);
                    }
                }
                _ => {}
            }
        }
        blocks
    }

    #[test]
    fn extracts_exact_fenced_code_content() {
        let markdown = "```rust\nfn main() {}\n```\n";
        let range = extract_code_block_content_range(markdown);
        assert_eq!(&markdown[range], "fn main() {}\n");

        let markdown = "~~~~ text\none\n~~~~";
        let range = extract_code_block_content_range(markdown);
        assert_eq!(&markdown[range], "one\n");
    }

    #[test]
    fn opener_only_fenced_code_blocks_have_empty_content() {
        for markdown in ["```\n", "```rust\n", "~~~\n", "~~~rust\n"] {
            let range = extract_code_block_content_range(markdown);
            assert_eq!(&markdown[range.clone()], "");
            assert_eq!(range, markdown.len()..markdown.len());
        }
    }

    #[test]
    fn extracts_incomplete_and_complete_fenced_code_content() {
        for (markdown, expected) in [
            ("```rust\nlet value", "let value"),
            ("```rust\nlet value\n", "let value\n"),
            ("```rust\nlet value\n```", "let value\n"),
            ("```rust\nlet value\n```\n", "let value\n"),
            ("  ```rust\n  value\n  ```\n", "  value\n"),
        ] {
            let range = extract_code_block_content_range(markdown);
            assert_eq!(&markdown[range], expected);
        }
    }

    #[test]
    fn extracts_crlf_fenced_code_content() {
        for (markdown, expected) in [
            ("```rust\r\n", ""),
            ("```rust\r\nlet value", "let value"),
            ("```rust\r\nlet value\r\n", "let value\r\n"),
            ("```rust\r\nlet value\r\n```\r\n", "let value\r\n"),
        ] {
            let range = extract_code_block_content_range(markdown);
            assert_eq!(&markdown[range], expected);
        }
    }

    #[test]
    fn opener_only_stream_does_not_panic_and_preserves_code_block_identity() {
        let opener = "```rust\n";
        let incomplete = "```rust\nlet value";
        let complete = "```rust\nlet value = 42;\n```";
        let opener_id = code_block_id(opener, &(0..opener.len()));
        let first = parsed_code_blocks(opener);
        let second = parsed_code_blocks(incomplete);
        let third = parsed_code_blocks(complete);

        assert_eq!(opener_id.0, 0..8);
        assert_eq!(first[0].0, opener_id);
        assert_eq!(second[0].0, opener_id);
        assert_eq!(third[0].0, opener_id);
        assert_eq!(third[0].1, "let value = 42;\n");
    }

    #[test]
    fn copied_code_uses_normalized_parser_text() {
        assert_eq!(
            parsed_code_blocks("    first\n    second\n")[0].1,
            "first\nsecond\n"
        );
        assert_eq!(
            parsed_code_blocks("> ```\n> first\n> second\n> ```")[0].1,
            "first\nsecond\n"
        );
    }

    #[test]
    fn selection_clamps_to_valid_source_boundaries() {
        let mut selection = Selection {
            start: 2,
            end: usize::MAX,
            reversed: false,
            pending: true,
            mode: SelectMode::Word(2..usize::MAX),
        };

        selection.clamp_to_source("aé");

        assert_eq!(selection.start, 1);
        assert_eq!(selection.end, 3);
        assert!(!selection.pending);
        assert!(matches!(selection.mode, SelectMode::Word(range) if range == (1..3)));
    }
}
