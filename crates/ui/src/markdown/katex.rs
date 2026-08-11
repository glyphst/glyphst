//! RaTeX-backed Markdown math rendering.
//!
//! The cache and fallback behavior intentionally follows Zed PR #57339. A
//! rendered image belongs to a formula occurrence as well as its contents, so
//! streaming edits can keep showing the preceding image while the replacement
//! is rendered without ever borrowing an image from a newly inserted formula.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use gpui::{
    AnyElement, App, Context, Hsla, ImageSource, IntoElement, RenderImage, Rgba, SharedString,
    StyledText, Task, TextStyle, WeakEntity, Window, div, img, prelude::*,
};
use gpui_component::text::{MarkdownExtensions, MarkdownNode, MarkdownParseContext, markdown_ast};
use ratex_layout::{LayoutOptions, layout, to_display_list};
use ratex_svg::{SvgOptions, render_to_svg};
use ratex_types::{color::Color, math_style::MathStyle};

use super::Markdown;

const MATH_NODE_NAME: &str = "glyphst-math";

type MathCache = HashMap<MathContents, Arc<CachedMath>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum MathMode {
    Inline,
    Display,
}

#[derive(Clone, Debug)]
struct MathNode {
    contents: SharedString,
    source: SharedString,
    mode: MathMode,
    source_offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MathContents {
    contents: SharedString,
    mode: MathMode,
    font_size: u32,
    color: [u8; 4],
}

#[derive(Clone, Debug)]
struct MathOccurrence {
    source_start: usize,
    contents: SharedString,
    mode: MathMode,
    rendered_key: Option<MathContents>,
}

struct CachedMath {
    render_image: Arc<OnceLock<Result<Arc<RenderImage>, SharedString>>>,
    fallback_image: Option<Arc<RenderImage>>,
    _task: Task<()>,
}

#[derive(Default)]
pub(super) struct KatexState {
    cache: MathCache,
    occurrences: Vec<MathOccurrence>,
}

impl KatexState {
    pub(super) fn update_source(&mut self, source: &str, extensions: &MarkdownExtensions) {
        let parsed_occurrences = extract_math_occurrences(source, extensions);
        let old_occurrences = std::mem::take(&mut self.occurrences);
        let can_migrate_rendered_keys = old_occurrences.len() == parsed_occurrences.len();

        self.occurrences = parsed_occurrences
            .into_iter()
            .enumerate()
            .map(|(index, (source_start, contents, mode))| MathOccurrence {
                source_start,
                contents,
                mode,
                rendered_key: can_migrate_rendered_keys
                    .then(|| old_occurrences[index].rendered_key.clone())
                    .flatten(),
            })
            .collect();

        Self::retain_active_cache_entries(&mut self.cache, &self.occurrences, true);
    }

    fn retain_active_cache_entries(
        cache: &mut MathCache,
        active_occurrences: &[MathOccurrence],
        retain_rendered_keys: bool,
    ) {
        cache.retain(|contents, _| {
            active_occurrences.iter().any(|occurrence| {
                (occurrence.contents == contents.contents && occurrence.mode == contents.mode)
                    || (retain_rendered_keys
                        && occurrence
                            .rendered_key
                            .as_ref()
                            .is_some_and(|rendered_key| rendered_key == contents))
            })
        });
    }

    fn fallback_image_for_source_start(&self, source_start: usize) -> Option<Arc<RenderImage>> {
        let rendered_key = self
            .occurrences
            .iter()
            .find(|occurrence| occurrence.source_start == source_start)?
            .rendered_key
            .as_ref()?;
        Self::cached_fallback_image(&self.cache, rendered_key)
    }

    fn cached_fallback_image(
        cache: &MathCache,
        rendered_key: &MathContents,
    ) -> Option<Arc<RenderImage>> {
        cache.get(rendered_key).and_then(|cached| {
            cached
                .render_image
                .get()
                .and_then(|result| result.as_ref().ok().cloned())
                .or_else(|| cached.fallback_image.clone())
        })
    }

    fn occurrence_for_source_start_mut(
        &mut self,
        source_start: usize,
    ) -> Option<&mut MathOccurrence> {
        self.occurrences
            .iter_mut()
            .find(|occurrence| occurrence.source_start == source_start)
    }

    fn ensure_cached_contents(
        &mut self,
        source_start: usize,
        contents: &MathContents,
        cx: &mut Context<Markdown>,
    ) {
        if !self.cache.contains_key(contents) {
            let fallback_image = self.fallback_image_for_source_start(source_start);
            self.cache.insert(
                contents.clone(),
                Arc::new(CachedMath::new(contents.clone(), fallback_image, cx)),
            );
        }

        if let Some(occurrence) = self.occurrence_for_source_start_mut(source_start) {
            occurrence.rendered_key = Some(contents.clone());
        }
        Self::retain_active_cache_entries(&mut self.cache, &self.occurrences, false);
    }

    fn render_content(
        &self,
        contents: &MathContents,
        source: &str,
        source_offset: usize,
    ) -> AnyElement {
        let cached = self.cache.get(contents);
        let render_result = cached.and_then(|cached| cached.render_image.get());

        match render_result {
            Some(Ok(image)) => render_math_image(image.clone(), contents.mode, source_offset),
            Some(Err(_)) => render_math_source(source),
            None => cached
                .and_then(|cached| cached.fallback_image.clone())
                .map(|image| render_math_image(image, contents.mode, source_offset))
                .unwrap_or_else(|| render_math_source(source)),
        }
    }
}

impl CachedMath {
    fn new(
        contents: MathContents,
        fallback_image: Option<Arc<RenderImage>>,
        cx: &mut Context<Markdown>,
    ) -> Self {
        let render_image = Arc::new(OnceLock::new());
        let result = render_image.clone();
        let svg_renderer = cx.svg_renderer();

        let task = cx.spawn(async move |this, cx| {
            let rendered = cx
                .background_spawn(async move { render_math_image_data(&contents, svg_renderer) })
                .await;
            let _ = result.set(rendered);
            let _ = this.update(cx, |markdown, cx| {
                markdown.text_view.update(cx, |_, cx| cx.notify());
                cx.notify();
            });
        });

        Self {
            render_image,
            fallback_image,
            _task: task,
        }
    }

    #[cfg(test)]
    fn new_for_test(
        render_image: Option<Arc<RenderImage>>,
        fallback_image: Option<Arc<RenderImage>>,
    ) -> Self {
        let result = Arc::new(OnceLock::new());
        if let Some(render_image) = render_image {
            let _ = result.set(Ok(render_image));
        }
        Self {
            render_image: result,
            fallback_image,
            _task: Task::ready(()),
        }
    }
}

pub(super) fn add_extensions(
    extensions: MarkdownExtensions,
    markdown: WeakEntity<Markdown>,
) -> MarkdownExtensions {
    let inline_markdown = markdown.clone();
    extensions
        .math()
        .inline_parser(parse_inline_math)
        .inline_renderer(MATH_NODE_NAME, move |node, text_style, window, cx| {
            render_math_node(inline_markdown.clone(), node, text_style, window, cx)
        })
        .block_parser(parse_display_math)
        .block_renderer(MATH_NODE_NAME, move |node, window, cx| {
            let text_style = window.text_style();
            render_math_node(markdown.clone(), node, &text_style, window, cx)
        })
}

fn node_offset(node: &markdown_ast::Node, cx: &MarkdownParseContext<'_>) -> usize {
    cx.offset() + node.position().map_or(0, |position| position.start.offset)
}

fn parse_inline_math(
    node: &markdown_ast::Node,
    cx: &MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::InlineMath(math) = node else {
        return None;
    };
    let source = cx
        .node_source(node)
        .map(str::to_string)
        .unwrap_or_else(|| format!("${}$", math.value));
    let mode = mode_for_inline_source(&source);
    let node = MarkdownNode::new(
        MATH_NODE_NAME,
        MathNode {
            contents: math.value.clone().into(),
            source: source.clone().into(),
            mode,
            source_offset: node_offset(node, cx),
        },
    )
    .text(math.value.clone())
    .markdown(source);
    Some(if mode == MathMode::Display {
        node.full_width()
    } else {
        node
    })
}

fn parse_display_math(
    node: &markdown_ast::Node,
    cx: &MarkdownParseContext<'_>,
) -> Option<MarkdownNode> {
    let markdown_ast::Node::Math(math) = node else {
        return None;
    };
    let source = cx
        .node_source(node)
        .map(str::to_string)
        .unwrap_or_else(|| format!("$$\n{}\n$$", math.value));
    Some(
        MarkdownNode::new(
            MATH_NODE_NAME,
            MathNode {
                contents: math.value.clone().into(),
                source: source.clone().into(),
                mode: MathMode::Display,
                source_offset: node_offset(node, cx),
            },
        )
        .text(math.value.clone())
        .markdown(source),
    )
}

fn render_math_node(
    markdown: WeakEntity<Markdown>,
    node: &MarkdownNode,
    text_style: &TextStyle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(math) = node.data::<MathNode>() else {
        return render_math_source(node.as_text());
    };
    let font_size = f32::from(text_style.font_size.to_pixels(window.rem_size()));
    let contents = cache_contents(math, font_size, text_style.color);

    let rendered = markdown
        .update(cx, |markdown, cx| {
            markdown
                .katex
                .ensure_cached_contents(math.source_offset, &contents, cx);
            markdown
                .katex
                .render_content(&contents, &math.source, math.source_offset)
        })
        .unwrap_or_else(|_| render_math_source(&math.source));

    match math.mode {
        MathMode::Inline => rendered,
        MathMode::Display => div()
            .flex()
            .w_full()
            .items_center()
            .justify_center()
            .py_2()
            .child(rendered)
            .into_any_element(),
    }
}

fn render_math_image(image: Arc<RenderImage>, mode: MathMode, source_offset: usize) -> AnyElement {
    let image = img(ImageSource::Render(image))
        .id(("markdown-math", source_offset))
        .debug_selector(|| "markdown-math".into());
    match mode {
        MathMode::Inline => image.flex_none().into_any_element(),
        MathMode::Display => image.max_w_full().into_any_element(),
    }
}

fn render_math_source(source: &str) -> AnyElement {
    StyledText::new(source.to_string()).into_any_element()
}

fn cache_contents(math: &MathNode, font_size: f32, color: Hsla) -> MathContents {
    let effective_font_size = match math.mode {
        MathMode::Inline => font_size * 0.83,
        MathMode::Display => font_size,
    };
    MathContents {
        contents: math.contents.clone(),
        mode: math.mode,
        font_size: (effective_font_size * 100.0).round() as u32,
        color: hsla_to_rgba_bytes(color),
    }
}

fn render_math_image_data(
    contents: &MathContents,
    svg_renderer: gpui::SvgRenderer,
) -> Result<Arc<RenderImage>, SharedString> {
    let svg = render_math_svg(contents)?;
    svg_renderer
        .render_single_frame(svg.as_bytes(), 1.0)
        .map_err(|error| SharedString::from(format!("failed to render math SVG: {error}")))
}

fn render_math_svg(contents: &MathContents) -> Result<String, SharedString> {
    let ast = ratex_parser::parse(contents.contents.as_ref())
        .map_err(|error| SharedString::from(format!("failed to parse math: {error}")))?;
    let layout_options = LayoutOptions::default()
        .with_style(math_style(contents.mode))
        .with_color(rgba_bytes_to_ratex_color(contents.color));
    let layout_box = layout(&ast, &layout_options);
    let display_list = to_display_list(&layout_box);
    Ok(render_to_svg(
        &display_list,
        &SvgOptions {
            font_size: contents.font_size as f64 / 100.0,
            padding: f64::from(padding_for_mode(contents.mode)),
            stroke_width: 1.0,
            embed_glyphs: true,
            ..SvgOptions::default()
        },
    ))
}

fn extract_math_occurrences(
    source: &str,
    extensions: &MarkdownExtensions,
) -> Vec<(usize, SharedString, MathMode)> {
    let Ok(root) = extensions.parse_ast(source) else {
        return Vec::new();
    };

    fn visit(
        node: &markdown_ast::Node,
        source: &str,
        occurrences: &mut Vec<(usize, SharedString, MathMode)>,
    ) {
        match node {
            markdown_ast::Node::InlineMath(math) => {
                if let Some(position) = node.position() {
                    let mode = source_for_position(node, source)
                        .map(mode_for_inline_source)
                        .unwrap_or(MathMode::Inline);
                    occurrences.push((position.start.offset, math.value.clone().into(), mode));
                }
            }
            markdown_ast::Node::Math(math) => {
                if let Some(position) = node.position() {
                    occurrences.push((
                        position.start.offset,
                        math.value.clone().into(),
                        MathMode::Display,
                    ));
                }
            }
            _ => {}
        }
        if let Some(children) = node.children() {
            for child in children {
                visit(child, source, occurrences);
            }
        }
    }

    let mut occurrences = Vec::new();
    visit(&root, source, &mut occurrences);
    occurrences.sort_by_key(|(source_start, _, _)| *source_start);
    occurrences
}

fn source_for_position<'a>(node: &markdown_ast::Node, source: &'a str) -> Option<&'a str> {
    let position = node.position()?;
    source.get(position.start.offset..position.end.offset)
}

fn mode_for_inline_source(source: &str) -> MathMode {
    if ((source.starts_with("$$") && source.ends_with("$$"))
        || (source.starts_with("\\[") && source.ends_with("\\]")))
        && source.len() >= 4
    {
        MathMode::Display
    } else {
        MathMode::Inline
    }
}

fn math_style(mode: MathMode) -> MathStyle {
    match mode {
        MathMode::Inline => MathStyle::Text,
        MathMode::Display => MathStyle::Display,
    }
}

fn padding_for_mode(mode: MathMode) -> f32 {
    match mode {
        MathMode::Inline => 0.5,
        MathMode::Display => 3.0,
    }
}

fn hsla_to_rgba_bytes(color: Hsla) -> [u8; 4] {
    let rgba: Rgba = color.to_rgb();
    [
        (rgba.r * 255.0).round() as u8,
        (rgba.g * 255.0).round() as u8,
        (rgba.b * 255.0).round() as u8,
        (rgba.a * 255.0).round() as u8,
    ]
}

fn rgba_bytes_to_ratex_color(color: [u8; 4]) -> Color {
    Color::new(
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        color[3] as f32 / 255.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Render, TestAppContext};

    fn mock_render_image(cx: &mut TestAppContext) -> Arc<RenderImage> {
        cx.update(|cx| {
            cx.svg_renderer()
                .render_single_frame(
                    br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>"#,
                    1.0,
                )
                .expect("test svg should render")
        })
    }

    fn math_contents(contents: &str, mode: MathMode, font_size: u32) -> MathContents {
        MathContents {
            contents: contents.into(),
            mode,
            font_size,
            color: [255, 255, 255, 255],
        }
    }

    fn seed_rendered_cache(
        state: &mut KatexState,
        source_start: usize,
        contents: MathContents,
        image: Arc<RenderImage>,
    ) {
        state.cache.insert(
            contents.clone(),
            Arc::new(CachedMath::new_for_test(Some(image), None)),
        );
        state
            .occurrence_for_source_start_mut(source_start)
            .expect("occurrence should exist")
            .rendered_key = Some(contents);
    }

    #[test]
    fn extracts_dollar_and_latex_math_occurrences() {
        let source = r"Inline $x^2$ and \(z\)

$$y$$

\[
w
\]

\[q\]";
        let extensions = MarkdownExtensions::default().math();
        let occurrences = extract_math_occurrences(source, &extensions);
        assert_eq!(occurrences.len(), 5);
        assert_eq!(occurrences[0].0, "Inline ".len());
        assert_eq!(occurrences[0].1, "x^2");
        assert_eq!(occurrences[0].2, MathMode::Inline);
        assert_eq!(occurrences[1].0, source.find(r"\(z\)").unwrap());
        assert_eq!(occurrences[1].1, "z");
        assert_eq!(occurrences[1].2, MathMode::Inline);
        assert_eq!(occurrences[2].1, "y");
        assert_eq!(occurrences[2].2, MathMode::Display);
        assert_eq!(occurrences[3].1, "w");
        assert_eq!(occurrences[3].2, MathMode::Display);
        assert_eq!(occurrences[4].1, "q");
        assert_eq!(occurrences[4].2, MathMode::Display);
    }

    #[test]
    fn extracts_display_formula_containing_markdown_heading_marker() {
        let source = r"\[
\boxed{
C_{\text{R-SWA}}(T)
=
L_m+\min(n,T)
\leq L_m+n
}
\tag{6}
\]";
        let extensions = MarkdownExtensions::default().math();
        let occurrences = extract_math_occurrences(source, &extensions);

        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].0, 0);
        assert_eq!(occurrences[0].2, MathMode::Display);
        assert_eq!(
            occurrences[0].1,
            r"\boxed{
C_{\text{R-SWA}}(T)
=
L_m+\min(n,T)
\leq L_m+n
}
\tag{6}"
        );
    }

    #[gpui::test]
    fn edited_formula_uses_its_previous_render_as_fallback(cx: &mut TestAppContext) {
        struct TestRoot;
        impl Render for TestRoot {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
            }
        }

        let (_, cx) = cx.add_window_view(|_, _| TestRoot);
        let markdown = cx.new(|cx| {
            Markdown::new_with_features(
                r"\(x\) \(y\) \(z\)".into(),
                super::super::MarkdownFeatures {
                    math: true,
                    mermaid: false,
                },
                cx,
            )
        });
        let y_start = r"\(x\) ".len();
        let old_y = math_contents("y", MathMode::Inline, 1_400);
        let new_y = math_contents("y^2", MathMode::Inline, 1_400);
        let image = mock_render_image(cx);

        markdown.update(cx, |markdown, cx| {
            seed_rendered_cache(&mut markdown.katex, y_start, old_y, image.clone());
            let extensions = markdown.extensions.clone();
            markdown
                .katex
                .update_source(r"\(x\) \(y^2\) \(z\)", &extensions);
            markdown.katex.ensure_cached_contents(y_start, &new_y, cx);
            assert_eq!(
                markdown
                    .katex
                    .cache
                    .get(&new_y)
                    .and_then(|cached| cached.fallback_image.as_ref())
                    .map(|fallback| fallback.id),
                Some(image.id)
            );
        });
    }

    #[gpui::test]
    fn inserted_formula_does_not_borrow_another_occurrence(cx: &mut TestAppContext) {
        struct TestRoot;
        impl Render for TestRoot {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
            }
        }

        let (_, cx) = cx.add_window_view(|_, _| TestRoot);
        let markdown = cx.new(|cx| {
            Markdown::new_with_features(
                "$x$ $z$".into(),
                super::super::MarkdownFeatures {
                    math: true,
                    mermaid: false,
                },
                cx,
            )
        });
        let old_z_start = "$x$ ".len();
        let old_z = math_contents("z", MathMode::Inline, 1_400);
        let new_y = math_contents("y", MathMode::Inline, 1_400);
        let image = mock_render_image(cx);

        markdown.update(cx, |markdown, cx| {
            seed_rendered_cache(&mut markdown.katex, old_z_start, old_z, image);
            let extensions = markdown.extensions.clone();
            markdown.katex.update_source("$x$ $y$ $z$", &extensions);
            markdown
                .katex
                .ensure_cached_contents(old_z_start, &new_y, cx);
            assert!(
                markdown
                    .katex
                    .cache
                    .get(&new_y)
                    .and_then(|cached| cached.fallback_image.as_ref())
                    .is_none()
            );
        });
    }

    #[test]
    fn ratex_renders_inline_and_display_svg() {
        for mode in [MathMode::Inline, MathMode::Display] {
            let svg = render_math_svg(&MathContents {
                contents: r"\sqrt{3x-1}+(1+x)^2".into(),
                mode,
                font_size: 1_400,
                color: [32, 48, 64, 255],
            })
            .expect("math should render");
            assert!(svg.contains("<svg"));
            assert!(svg.contains("<path"));
        }

        let svg = render_math_svg(&MathContents {
            contents: r"\boxed{
C_{\text{R-SWA}}(T)
=
L_m+\min(n,T)
\leq L_m+n
}
\tag{6}"
                .into(),
            mode: MathMode::Display,
            font_size: 1_400,
            color: [32, 48, 64, 255],
        })
        .expect("AI-style display formula should render");
        assert!(svg.contains("<svg"));
        assert!(svg.contains("<path"));
    }
}
