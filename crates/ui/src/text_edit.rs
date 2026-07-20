use std::{
    collections::VecDeque,
    ops::Range,
    time::{Duration, Instant},
};

use gpui::SharedString;
use unicode_segmentation::UnicodeSegmentation as _;

const DEFAULT_HISTORY_CAPACITY: usize = 100;
const EDIT_GROUP_TIMEOUT: Duration = Duration::from_millis(1_000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditGroup {
    Typing,
    Backspace,
    Delete,
    Composition,
    Isolated,
}

impl EditGroup {
    fn coalesces(self) -> bool {
        !matches!(self, Self::Isolated)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextSnapshot {
    pub content: SharedString,
    pub selected_range: Range<usize>,
    pub selection_reversed: bool,
    pub marked_range: Option<Range<usize>>,
}

impl TextSnapshot {
    pub fn new(
        content: SharedString,
        selected_range: Range<usize>,
        selection_reversed: bool,
        marked_range: Option<Range<usize>>,
    ) -> Self {
        Self {
            content,
            selected_range,
            selection_reversed,
            marked_range,
        }
    }
}

#[derive(Clone, Copy)]
struct ActiveGroup {
    kind: EditGroup,
    last_edit: Instant,
}

pub struct EditHistory<S> {
    undo: VecDeque<S>,
    redo: Vec<S>,
    active_group: Option<ActiveGroup>,
    capacity: usize,
}

impl<S> Default for EditHistory<S> {
    fn default() -> Self {
        Self::new(DEFAULT_HISTORY_CAPACITY)
    }
}

impl<S> EditHistory<S> {
    pub fn new(capacity: usize) -> Self {
        Self {
            undo: VecDeque::with_capacity(capacity),
            redo: Vec::new(),
            active_group: None,
            capacity,
        }
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.active_group = None;
    }

    pub fn break_group(&mut self) {
        self.active_group = None;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

impl<S: PartialEq> EditHistory<S> {
    pub fn record(&mut self, before: S, after: &S, group: EditGroup) {
        if &before == after {
            return;
        }

        let now = Instant::now();
        let coalesce = group.coalesces()
            && self.active_group.is_some_and(|active| {
                active.kind == group
                    && now.saturating_duration_since(active.last_edit) <= EDIT_GROUP_TIMEOUT
            });

        if !coalesce && self.capacity > 0 {
            if self.undo.len() == self.capacity {
                self.undo.pop_front();
            }
            self.undo.push_back(before);
        }

        self.redo.clear();
        self.active_group = group.coalesces().then_some(ActiveGroup {
            kind: group,
            last_edit: now,
        });
    }

    pub fn undo(&mut self, current: S) -> Option<S> {
        let previous = self.undo.pop_back()?;
        self.redo.push(current);
        self.active_group = None;
        Some(previous)
    }

    pub fn redo(&mut self, current: S) -> Option<S> {
        let next = self.redo.pop()?;
        if self.capacity > 0 {
            if self.undo.len() == self.capacity {
                self.undo.pop_front();
            }
            self.undo.push_back(current);
        }
        self.active_group = None;
        Some(next)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharacterClass {
    Whitespace,
    Punctuation,
    Word,
}

fn character_class(ch: char) -> CharacterClass {
    if ch.is_whitespace() {
        CharacterClass::Whitespace
    } else if ch.is_alphanumeric() || ch == '_' {
        CharacterClass::Word
    } else {
        CharacterClass::Punctuation
    }
}

pub fn previous_grapheme_boundary(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset);
    text.grapheme_indices(true)
        .rev()
        .find_map(|(index, _)| (index < offset).then_some(index))
        .unwrap_or(0)
}

pub fn next_grapheme_boundary(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset);
    text.grapheme_indices(true)
        .find_map(|(index, _)| (index > offset).then_some(index))
        .unwrap_or(text.len())
}

pub fn previous_word_start(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset);
    if offset == 0 {
        return 0;
    }

    let chars = text[..offset].char_indices().collect::<Vec<_>>();
    let mut index = chars.len();
    while index > 0 && chars[index - 1].1.is_whitespace() {
        index -= 1;
    }
    if index == 0 {
        return 0;
    }

    let class = character_class(chars[index - 1].1);
    while index > 0 {
        let ch = chars[index - 1].1;
        if character_class(ch) != class {
            break;
        }
        index -= 1;
    }
    chars.get(index).map_or(0, |(offset, _)| *offset)
}

pub fn next_word_end(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset);
    if offset >= text.len() {
        return text.len();
    }

    let chars = text[offset..]
        .char_indices()
        .map(|(index, ch)| (offset + index, ch))
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() && chars[index].1.is_whitespace() {
        index += 1;
    }
    if index == chars.len() {
        return text.len();
    }

    let class = character_class(chars[index].1);
    while index < chars.len() {
        let ch = chars[index].1;
        if character_class(ch) != class {
            break;
        }
        index += 1;
    }
    chars.get(index).map_or(text.len(), |(offset, _)| *offset)
}

pub fn backward_word_deletion_start(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset);
    let previous = previous_grapheme_boundary(text, offset);
    if text[previous..offset].contains('\n') {
        return previous;
    }
    let start = line_start(text, offset);
    let target = start + previous_word_start(&text[start..offset], offset - start);
    adjust_greedy_deletion(text, target..offset, true)
}

pub fn forward_word_deletion_end(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset);
    let next = next_grapheme_boundary(text, offset);
    if text[offset..next].contains('\n') {
        return next;
    }
    let end = line_end(text, offset);
    let target = offset + next_word_end(&text[offset..end], 0);
    adjust_greedy_deletion(text, offset..target, false)
}

fn adjust_greedy_deletion(text: &str, range: Range<usize>, backward: bool) -> usize {
    if range.is_empty() {
        return if backward { range.start } else { range.end };
    }

    let mut whitespace_runs = Vec::new();
    let mut run_start = None;
    let mut run_len = 0;
    for (relative, ch) in text[range.clone()].char_indices() {
        let offset = range.start + relative;
        if ch.is_whitespace() && ch != '\n' {
            run_start.get_or_insert(offset);
            run_len += 1;
        } else if let Some(start) = run_start.take() {
            if run_len >= 2 {
                whitespace_runs.push(start..offset);
            }
            run_len = 0;
        }
    }
    if let Some(start) = run_start {
        let end = range.end;
        if run_len >= 2 {
            whitespace_runs.push(start..end);
        }
    }

    if backward {
        whitespace_runs
            .last()
            .map_or(range.start, |whitespace| whitespace.start)
    } else {
        whitespace_runs
            .first()
            .map_or(range.end, |whitespace| whitespace.end)
    }
}

pub fn line_start(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset);
    text[..offset].rfind('\n').map_or(0, |index| index + 1)
}

pub fn line_end(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset);
    text[offset..]
        .find('\n')
        .map_or(text.len(), |index| offset + index)
}

pub fn offset_from_utf16(text: &str, offset: usize) -> usize {
    let mut utf8_offset = 0;
    let mut utf16_count = 0;
    for ch in text.chars() {
        if utf16_count >= offset {
            break;
        }
        utf16_count += ch.len_utf16();
        utf8_offset += ch.len_utf8();
    }
    utf8_offset
}

pub fn offset_to_utf16(text: &str, offset: usize) -> usize {
    let offset = floor_char_boundary(text, offset);
    text[..offset].encode_utf16().count()
}

pub fn range_from_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    offset_from_utf16(text, range.start)..offset_from_utf16(text, range.end)
}

pub fn range_to_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    offset_to_utf16(text, range.start)..offset_to_utf16(text, range.end)
}

fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grapheme_boundaries_do_not_split_user_perceived_characters() {
        let text = "a\u{301}👨‍👩‍👧‍👦中";
        let family_start = "a\u{301}".len();
        let cjk_start = family_start + "👨‍👩‍👧‍👦".len();

        assert_eq!(next_grapheme_boundary(text, 0), family_start);
        assert_eq!(next_grapheme_boundary(text, family_start), cjk_start);
        assert_eq!(previous_grapheme_boundary(text, cjk_start), family_start);
        assert_eq!(previous_grapheme_boundary(text, text.len()), cjk_start);
    }

    #[test]
    fn word_boundaries_distinguish_words_punctuation_and_whitespace() {
        let text = "hello...  world_2\nnext";
        assert_eq!(next_word_end(text, 0), 5);
        assert_eq!(next_word_end(text, 5), 8);
        assert_eq!(next_word_end(text, 8), 17);
        assert_eq!(previous_word_start(text, 17), 10);
        assert_eq!(previous_word_start(text, 10), 5);
        assert_eq!(next_word_end(text, 17), 22);
        assert_eq!(previous_word_start(text, 18), 10);
        assert_eq!(next_word_end(text, 18), 22);
    }

    #[test]
    fn word_deletion_stops_at_long_whitespace_runs() {
        let text = "hello    world";
        assert_eq!(backward_word_deletion_start(text, 9), 5);
        assert_eq!(forward_word_deletion_end(text, 5), 9);
        assert_eq!(backward_word_deletion_start(text, text.len()), 9);

        let multiline = "hello\nworld";
        assert_eq!(backward_word_deletion_start(multiline, 6), 5);
        assert_eq!(forward_word_deletion_end(multiline, 5), 6);
    }

    #[test]
    fn utf16_conversion_handles_non_bmp_characters() {
        let text = "a😀中";
        assert_eq!(offset_from_utf16(text, 3), "a😀".len());
        assert_eq!(offset_to_utf16(text, "a😀".len()), 3);
    }

    #[test]
    fn history_coalesces_matching_edit_groups() {
        let mut history = EditHistory::new(10);
        history.record(0, &1, EditGroup::Typing);
        history.record(1, &2, EditGroup::Typing);
        assert_eq!(history.undo(2), Some(0));
        assert_eq!(history.redo(0), Some(2));
    }

    #[test]
    fn history_breaks_groups_and_invalidates_redo() {
        let mut history = EditHistory::new(10);
        history.record(0, &1, EditGroup::Typing);
        history.break_group();
        history.record(1, &2, EditGroup::Typing);
        assert_eq!(history.undo(2), Some(1));
        assert_eq!(history.undo(1), Some(0));
        assert_eq!(history.redo(0), Some(1));
        history.record(1, &3, EditGroup::Isolated);
        assert!(!history.can_redo());
    }
}
