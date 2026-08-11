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
    StyledExt as _, ThemeColor,
    clipboard::Clipboard,
    highlighter::HighlightTheme,
    text::{
        MarkdownExtensions, TextView, TextViewContextMenuRequest, TextViewState, TextViewStyle,
    },
};

use glyphst_settings::localize::tr;

mod embedded;
mod katex;

/// Optional embedded Markdown renderers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MarkdownFeatures {
    pub mermaid: bool,
    pub math: bool,
}

impl MarkdownFeatures {
    /// Enable the embedded content supported in AI messages.
    pub const fn embedded_content() -> Self {
        Self {
            mermaid: true,
            math: true,
        }
    }
}

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
    pub fn from_theme(theme: ThemeColor, base_text_style: TextStyle) -> Self {
        let is_dark = theme.background.to_rgb().r * 0.2126
            + theme.background.to_rgb().g * 0.7152
            + theme.background.to_rgb().b * 0.0722
            < 0.5;
        let text_view_style = TextViewStyle {
            is_dark,
            highlight_theme: if is_dark {
                HighlightTheme::default_dark()
            } else {
                HighlightTheme::default_light()
            },
            ..Default::default()
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
    features: MarkdownFeatures,
    extensions: MarkdownExtensions,
    embedded: embedded::EmbeddedMarkdown,
    katex: katex::KatexState,
}

impl Markdown {
    pub fn new(source: SharedString, cx: &mut Context<Self>) -> Self {
        Self::new_with_features(source, MarkdownFeatures::default(), cx)
    }

    pub fn new_with_features(
        source: SharedString,
        features: MarkdownFeatures,
        cx: &mut Context<Self>,
    ) -> Self {
        let text_view = cx.new(|cx| TextViewState::markdown(source.as_ref(), cx));
        let extensions = embedded::extensions(features, cx.weak_entity());
        let mut katex = katex::KatexState::default();
        if features.math {
            katex.update_source(source.as_ref(), &extensions);
        }
        Self {
            source,
            text_view,
            features,
            extensions,
            embedded: embedded::EmbeddedMarkdown::default(),
            katex,
        }
    }

    pub fn source(&self) -> &SharedString {
        &self.source
    }

    pub fn features(&self) -> MarkdownFeatures {
        self.features
    }

    pub fn set_source(&mut self, source: SharedString, cx: &mut Context<Self>) {
        if self.source == source {
            return;
        }

        self.text_view
            .update(cx, |state, cx| state.set_text(source.as_ref(), cx));
        if self.features.math {
            self.katex.update_source(source.as_ref(), &self.extensions);
        }
        self.source = source;
        self.embedded.source_changed();
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
            .style(self.style.text_view_style)
            .markdown_extensions(self.markdown.read(cx).extensions.clone());

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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext, VisualTestContext, Window, px};
    use gpui_component::ActiveTheme as _;

    #[gpui::test]
    fn source_updates_component_text_view(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let markdown = cx.new(|cx| Markdown::new("before".into(), cx));

        markdown.update(cx, |markdown, cx| {
            assert_eq!(markdown.features(), MarkdownFeatures::default());
            markdown.set_source("after".into(), cx);
            assert_eq!(markdown.source().as_ref(), "after");
            assert!(markdown.selected_text(cx).is_none());
        });
    }

    #[gpui::test]
    fn embedded_content_preset_enables_math_and_mermaid(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let markdown = cx.new(|cx| {
            Markdown::new_with_features("$x$".into(), MarkdownFeatures::embedded_content(), cx)
        });

        markdown.read_with(cx, |markdown, _| {
            assert_eq!(
                markdown.features(),
                MarkdownFeatures {
                    mermaid: true,
                    math: true,
                }
            );
        });
    }

    #[gpui::test]
    fn inline_math_is_contained_by_its_narrow_markdown_layout(cx: &mut TestAppContext) {
        const SOURCE: &str = r#"This sentence uses `$` delimiters to show math inline: $\sqrt{3x-1}+(1+x)^2$ after."#;

        struct MathLayoutRoot {
            markdown: Entity<Markdown>,
        }

        impl Render for MathLayoutRoot {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let mut base_text_style = window.text_style();
                base_text_style.font_size = px(13.).into();
                let style = MarkdownStyle::from_theme(cx.theme().colors, base_text_style);
                div()
                    .w(px(350.))
                    .child(
                        div()
                            .debug_selector(|| "math-layout-body".into())
                            .w_full()
                            .px_2()
                            .child(MarkdownElement::new(self.markdown.clone(), style)),
                    )
                    .child(
                        div()
                            .debug_selector(|| "math-layout-footer".into())
                            .h(px(1.)),
                    )
            }
        }

        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let content = cx.new(|cx| MathLayoutRoot {
                markdown: cx.new(|cx| {
                    Markdown::new_with_features(
                        SOURCE.into(),
                        MarkdownFeatures {
                            math: true,
                            mermaid: false,
                        },
                        cx,
                    )
                }),
            });
            gpui_component::Root::new(content, window, cx)
        });
        let cx: &mut VisualTestContext = cx;

        // The first draw starts the asynchronous RaTeX render; the second
        // verifies layout using the resulting intrinsic image dimensions.
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let formula = cx
            .debug_bounds("markdown-math")
            .expect("rendered inline formula");
        let body = cx.debug_bounds("math-layout-body").unwrap();
        let footer = cx.debug_bounds("math-layout-footer").unwrap();
        assert!(
            formula.left() >= body.left()
                && formula.right() <= body.right()
                && formula.bottom() <= body.bottom(),
            "formula escaped the padded Markdown body: formula={formula:?}, body={body:?}"
        );
        assert!(
            formula.bottom() <= footer.top(),
            "formula overlapped content after the Markdown message: formula={formula:?}, footer={footer:?}"
        );
    }

    #[gpui::test]
    fn ai_style_math_message_stays_within_layout_after_render(cx: &mut TestAppContext) {
        const SOURCE: &str = r#"## Most important formula

The paper’s central result is the **bounded KV-cache size** of Reference Sliding Window Attention (R-SWA):

\[
\boxed{
C_{\text{R-SWA}}(T)
=
L_m+\min(n,T)
\leq L_m+n
}
\tag{6}
\]

where:

- \(T\): number of generated output tokens,
- \(L_m\): fixed prefix length containing the visual and prompt tokens,
- \(n\): fixed sliding-window width over recently generated tokens.

### Why it matters

With standard multi-head attention, the cache grows linearly:

\[
C_{\text{MHA}}(T)=L_m+T.
\]

R-SWA instead keeps **all fixed reference tokens but only the latest \(n\) output tokens**. Thus, once \(T\ge n\), its KV cache stays capped at \(L_m+n\), regardless of how long generation continues. This is the mechanism that enables the paper’s “unlimited” long-horizon OCR claim (pp. 6–7)."#;

        struct MathMessageLayoutRoot {
            markdown: Entity<Markdown>,
        }

        impl Render for MathMessageLayoutRoot {
            fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let mut base_text_style = window.text_style();
                base_text_style.font_family = ".ZedMono".into();
                base_text_style.font_size = px(17.).into();
                base_text_style.line_height = px(29.).into();
                let style = MarkdownStyle::from_theme(cx.theme().colors, base_text_style);

                div()
                    .w(px(390.))
                    .child(
                        div()
                            .debug_selector(|| "math-message-layout-body".into())
                            .w_full()
                            .px_2()
                            .child(MarkdownElement::new(self.markdown.clone(), style)),
                    )
                    .child(
                        div()
                            .debug_selector(|| "math-message-layout-footer".into())
                            .h(px(1.)),
                    )
            }
        }

        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let content = cx.new(|cx| MathMessageLayoutRoot {
                markdown: cx.new(|cx| {
                    Markdown::new_with_features(
                        SOURCE.into(),
                        MarkdownFeatures {
                            math: true,
                            mermaid: false,
                        },
                        cx,
                    )
                }),
            });
            gpui_component::Root::new(content, window, cx)
        });
        let cx: &mut VisualTestContext = cx;

        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let body = cx.debug_bounds("math-message-layout-body").unwrap();
        let footer = cx.debug_bounds("math-message-layout-footer").unwrap();
        let formula = cx
            .debug_bounds("markdown-math")
            .expect("rendered formula in AI-style message");
        assert!(
            formula.left() >= body.left() - px(0.5)
                && formula.right() <= body.right() + px(0.5)
                && formula.top() >= body.top() - px(0.5)
                && formula.bottom() <= body.bottom() + px(0.5),
            "rendered formula escaped the AI-message body: formula={formula:?}, body={body:?}"
        );
        assert!(
            body.bottom() <= footer.top(),
            "AI-style math message overlapped following content: body={body:?}, footer={footer:?}"
        );
    }
}
