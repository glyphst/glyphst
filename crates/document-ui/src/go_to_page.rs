use crate::{
    document_view::DocumentView,
    picker::{Picker, PickerDelegate},
};
use gpui::{AnyElement, Context, FocusHandle, SharedString, WeakEntity, Window, div, prelude::*};
use gpui_component::ThemeColor;

const PROMPT_WIDTH: f32 = 360.0;
const PROMPT_ROW_HEIGHT: f32 = 44.0;

pub type GoToPagePrompt = Picker<GoToPagePromptDelegate>;

pub struct GoToPagePromptDelegate {
    document_view: WeakEntity<DocumentView>,
    page_count: usize,
}

impl GoToPagePromptDelegate {
    pub fn new(document_view: WeakEntity<DocumentView>, page_count: usize) -> Self {
        Self {
            document_view,
            page_count,
        }
    }
}

fn parse_page_number(input: &str, page_count: usize) -> Option<usize> {
    let input = input.trim();
    let input = input.strip_prefix(':').unwrap_or(input).trim();
    let page = input.parse::<usize>().ok()?;

    (1..=page_count).contains(&page).then_some(page)
}

impl PickerDelegate for GoToPagePromptDelegate {
    fn key_context(&self) -> &'static str {
        "GoToPage Menu"
    }

    fn placeholder(&self) -> SharedString {
        crate::fl!("go-to-page-placeholder").into()
    }

    fn empty_message(&self) -> SharedString {
        crate::fl!("go-to-page-empty", page_count = self.page_count as i64).into()
    }

    fn width(&self) -> f32 {
        PROMPT_WIDTH
    }

    fn row_height(&self) -> f32 {
        PROMPT_ROW_HEIGHT
    }

    fn max_visible_matches(&self) -> usize {
        0
    }

    fn match_count(&self) -> usize {
        0
    }

    fn update_matches(&mut self, _: &str) {}

    fn render_match(&self, _: usize, _: ThemeColor, _: &FocusHandle, _: &Window) -> AnyElement {
        div().into_any_element()
    }

    fn confirm(
        &mut self,
        _: usize,
        _: &FocusHandle,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) {
    }

    fn confirm_input(
        &mut self,
        input: SharedString,
        _: &FocusHandle,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let Some(page_number) = parse_page_number(input.as_ref(), self.page_count) else {
            return;
        };

        if let Err(error) = self.document_view.update(cx, |document_view, cx| {
            document_view.navigate_to_page_number(page_number, cx);
        }) {
            eprintln!("failed to navigate to page: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_page_number;

    #[test]
    fn parses_valid_page_numbers() {
        assert_eq!(parse_page_number("1", 12), Some(1));
        assert_eq!(parse_page_number(" 12 ", 12), Some(12));
        assert_eq!(parse_page_number(":7", 12), Some(7));
        assert_eq!(parse_page_number(": 7", 12), Some(7));
    }

    #[test]
    fn rejects_invalid_page_numbers() {
        assert_eq!(parse_page_number("", 12), None);
        assert_eq!(parse_page_number(":", 12), None);
        assert_eq!(parse_page_number("0", 12), None);
        assert_eq!(parse_page_number("13", 12), None);
        assert_eq!(parse_page_number("7x", 12), None);
        assert_eq!(parse_page_number("1", 0), None);
    }
}
