use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    rc::Rc,
};

use gpui::{
    App, Context, Entity, IntoElement, RenderOnce, SharedString, StyleRefinement, TextStyle,
    Window, div, prelude::*,
};
use gpui_component::{
    StyledExt as _,
    clipboard::Clipboard,
    highlighter::HighlightTheme,
    text::{TextView, TextViewContextMenuRequest, TextViewState, TextViewStyle},
};

use crate::theme::Theme;
use glyphst_settings::localize::tr;

/// Glyphst's compatibility name for gpui-component's context-menu payload.
///
/// Keeping the alias lets existing consumers migrate without wrapping or
/// translating the component event.
pub type MarkdownContextMenuRequest = TextViewContextMenuRequest;

/// Presentation options retained by Glyphst's thin Markdown adapter.
///
/// Parsing, layout, selection, links, tables, syntax highlighting, and code
/// block presentation are all provided by [`TextView`].
pub struct MarkdownStyle {
    pub base_text_style: TextStyle,
    pub container_style: StyleRefinement,
    pub code_block_actions: bool,
    text_view_style: TextViewStyle,
}

impl MarkdownStyle {
    pub fn from_theme(theme: Theme, base_text_style: TextStyle) -> Self {
        let is_dark = background_is_dark(theme.background);
        let mut text_view_style = TextViewStyle::default();
        text_view_style.is_dark = is_dark;
        text_view_style.highlight_theme = if is_dark {
            HighlightTheme::default_dark()
        } else {
            HighlightTheme::default_light()
        };

        Self {
            base_text_style,
            container_style: StyleRefinement::default(),
            code_block_actions: false,
            text_view_style,
        }
    }

    pub fn with_code_block_actions(mut self, enabled: bool) -> Self {
        self.code_block_actions = enabled;
        self
    }
}

type ContextMenuCallback = Rc<dyn Fn(MarkdownContextMenuRequest, &mut Window, &mut App) + 'static>;

/// Mutable source state backed directly by gpui-component's [`TextViewState`].
pub struct Markdown {
    source: SharedString,
    text_view: Entity<TextViewState>,
}

impl Markdown {
    pub fn new(source: SharedString, cx: &mut Context<Self>) -> Self {
        let text_view = cx.new(|cx| TextViewState::markdown(source.as_ref(), cx));
        Self { source, text_view }
    }

    pub fn source(&self) -> &SharedString {
        &self.source
    }

    pub fn set_source(&mut self, source: SharedString, cx: &mut Context<Self>) {
        if self.source == source {
            return;
        }

        self.text_view
            .update(cx, |state, cx| state.set_text(source.as_ref(), cx));
        self.source = source;
        cx.notify();
    }

    pub fn selected_text(&self, cx: &App) -> Option<String> {
        let selected = self.text_view.read(cx).selected_text();
        (!selected.is_empty()).then_some(selected)
    }
}

/// Compatibility element that delegates rendering and interaction to
/// gpui-component's [`TextView`].
#[derive(IntoElement)]
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
        self.on_context_menu = Some(Rc::new(callback));
        self
    }
}

impl RenderOnce for MarkdownElement {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let text_view_state = self.markdown.read(cx).text_view.clone();
        let mut text_view = TextView::new(&text_view_state)
            .selectable(true)
            .style(self.style.text_view_style);

        if self.style.code_block_actions {
            let tooltip: SharedString = tr("markdown-copy-code").into();
            text_view = text_view.code_block_actions(move |code_block, _, _| {
                let code = code_block.code();
                let mut hasher = DefaultHasher::new();
                code.hash(&mut hasher);
                code_block.lang().hash(&mut hasher);

                Clipboard::new(("markdown-code-copy", hasher.finish()))
                    .value(code)
                    .tooltip(tooltip.clone())
            });
        }

        if let Some(on_context_menu) = self.on_context_menu {
            text_view = text_view.on_context_menu(move |request, window, cx| {
                on_context_menu(request, window, cx);
            });
        }

        div()
            .refine_style(&base_text_style_refinement(
                self.style.container_style,
                self.style.base_text_style,
            ))
            .child(text_view)
    }
}

fn base_text_style_refinement(mut container: StyleRefinement, base: TextStyle) -> StyleRefinement {
    container.text.color = Some(base.color);
    container.text.font_family = Some(base.font_family);
    container.text.font_features = Some(base.font_features);
    container.text.font_fallbacks = base.font_fallbacks;
    container.text.font_size = Some(base.font_size);
    container.text.line_height = Some(base.line_height);
    container.text.font_weight = Some(base.font_weight);
    container.text.font_style = Some(base.font_style);
    container.text.background_color = base.background_color;
    container.text.underline = base.underline;
    container.text.strikethrough = base.strikethrough;
    container.text.white_space = Some(base.white_space);
    container.text.text_overflow = base.text_overflow;
    container.text.text_align = Some(base.text_align);
    container.text.line_clamp = base.line_clamp;
    container
}

fn background_is_dark(rgb: u32) -> bool {
    let red = ((rgb >> 16) & 0xff) as f32;
    let green = ((rgb >> 8) & 0xff) as f32;
    let blue = (rgb & 0xff) as f32;
    red * 0.2126 + green * 0.7152 + blue * 0.0722 < 128.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn source_updates_component_text_view(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let markdown = cx.new(|cx| Markdown::new("before".into(), cx));

        markdown.update(cx, |markdown, cx| {
            markdown.set_source("after".into(), cx);
            assert_eq!(markdown.source().as_ref(), "after");
            assert!(markdown.selected_text(cx).is_none());
        });
    }

    #[test]
    fn detects_dark_backgrounds() {
        assert!(background_is_dark(0x101010));
        assert!(!background_is_dark(0xf0f0f0));
    }
}
