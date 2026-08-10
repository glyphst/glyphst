use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};

use glyphst_mermaid_render::{AccentColor, MermaidTheme};
use gpui::{
    AnyElement, App, Context, Entity, Hsla, ImageSource, IntoElement, ObjectFit, RenderImage, Rgba,
    SharedString, StyledText, Task, TextStyle, WeakEntity, Window, div, img, prelude::*,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _, ThemeColor,
    button::{Button, ButtonVariants as _},
    clipboard::Clipboard,
    h_flex,
    text::{MarkdownExtensions, MarkdownNode, MarkdownParseContext, TextViewState, markdown_ast},
    v_flex,
};
use ratex_layout::{LayoutOptions, layout, to_display_list};
use ratex_svg::{SvgOptions, render_to_svg};
use ratex_types::{color::Color, math_style::MathStyle};

use super::{Markdown, MarkdownFeatures};

const MATH_NODE_NAME: &str = "glyphst-math";
const MERMAID_NODE_NAME: &str = "glyphst-mermaid";

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

#[derive(Clone, Debug)]
struct MermaidNode {
    contents: SharedString,
    scale: u32,
    source_offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MathKey {
    contents: SharedString,
    mode: MathMode,
    font_size: u32,
    font_family: SharedString,
    color: [u8; 4],
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MermaidKey {
    contents: SharedString,
    scale: u32,
    font_family: SharedString,
    theme: [[u8; 4]; 16],
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum RenderKey {
    Math(MathKey),
    Mermaid(MermaidKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RenderKind {
    Math,
    Mermaid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct RenderOccurrence {
    kind: RenderKind,
    source_offset: usize,
}

struct RenderEntry {
    result: Arc<OnceLock<Result<Arc<RenderImage>, SharedString>>>,
    fallback: Option<Arc<RenderImage>>,
    _task: Task<()>,
}

#[derive(Clone)]
enum RenderSnapshot {
    Image(Arc<RenderImage>),
    Source,
}

#[derive(Default)]
pub(super) struct EmbeddedMarkdown {
    cache: HashMap<RenderKey, RenderEntry>,
    last_rendered: HashMap<RenderOccurrence, RenderKey>,
    mermaid_code_tabs: HashSet<usize>,
}

impl EmbeddedMarkdown {
    pub(super) fn source_changed(&mut self) {
        // Keep the last render for each source position as a streaming/theme
        // fallback. Bound the cache so deleted formulas and diagrams cannot
        // accumulate indefinitely across a long conversation.
        self.prune_cache();
    }

    fn prune_cache(&mut self) {
        if self.cache.len() <= 192 {
            return;
        }

        let active = self.last_rendered.values().cloned().collect::<HashSet<_>>();
        self.cache
            .retain(|key, entry| active.contains(key) || entry.result.get().is_none());
        if self.cache.len() > 256 {
            self.last_rendered.clear();
            self.cache.retain(|_, entry| entry.result.get().is_none());
        }
    }

    fn snapshot(&self, key: &RenderKey) -> RenderSnapshot {
        let Some(entry) = self.cache.get(key) else {
            return RenderSnapshot::Source;
        };
        match entry.result.get() {
            Some(Ok(image)) => RenderSnapshot::Image(image.clone()),
            Some(Err(_)) => RenderSnapshot::Source,
            None => entry
                .fallback
                .clone()
                .map(RenderSnapshot::Image)
                .unwrap_or(RenderSnapshot::Source),
        }
    }

    fn fallback_for(&self, occurrence: RenderOccurrence) -> Option<Arc<RenderImage>> {
        let old_key = self.last_rendered.get(&occurrence)?;
        let entry = self.cache.get(old_key)?;
        entry
            .result
            .get()
            .and_then(|result| result.as_ref().ok().cloned())
            .or_else(|| entry.fallback.clone())
    }

    fn ensure_rendered<F>(
        &mut self,
        key: RenderKey,
        occurrence: RenderOccurrence,
        text_view: Entity<TextViewState>,
        render: F,
        cx: &mut Context<Markdown>,
    ) -> RenderSnapshot
    where
        F: FnOnce() -> Result<Arc<RenderImage>, SharedString> + Send + 'static,
    {
        if !self.cache.contains_key(&key) {
            let fallback = self.fallback_for(occurrence);
            let result = Arc::new(OnceLock::new());
            let result_for_task = result.clone();
            let task = cx.spawn(async move |this, cx| {
                let rendered = cx.background_spawn(async move { render() }).await;
                let _ = result_for_task.set(rendered);
                let _ = this.update(cx, |_, cx| {
                    text_view.update(cx, |_, cx| cx.notify());
                    cx.notify();
                });
            });
            self.cache.insert(
                key.clone(),
                RenderEntry {
                    result,
                    fallback,
                    _task: task,
                },
            );
        }

        self.last_rendered.insert(occurrence, key.clone());
        self.prune_cache();
        self.snapshot(&key)
    }
}

pub(super) fn extensions(
    features: MarkdownFeatures,
    markdown: WeakEntity<Markdown>,
) -> MarkdownExtensions {
    let mut extensions = MarkdownExtensions::default();

    if features.math {
        let inline_markdown = markdown.clone();
        let display_markdown = markdown.clone();
        extensions = extensions
            .math()
            .inline_parser(parse_inline_math)
            .inline_renderer(MATH_NODE_NAME, move |node, text_style, window, cx| {
                render_math_node(inline_markdown.clone(), node, text_style, window, cx)
            })
            .block_parser(parse_display_math)
            .block_renderer(MATH_NODE_NAME, move |node, window, cx| {
                let text_style = window.text_style();
                render_math_node(display_markdown.clone(), node, &text_style, window, cx)
            });
    }

    if features.mermaid {
        let render_markdown = markdown;
        extensions = extensions
            .block_parser(parse_mermaid)
            .block_renderer(MERMAID_NODE_NAME, move |node, window, cx| {
                render_mermaid_node(render_markdown.clone(), node, window, cx)
            });
    }

    extensions
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
    Some(
        MarkdownNode::new(
            MATH_NODE_NAME,
            MathNode {
                contents: math.value.clone().into(),
                source: source.clone().into(),
                mode: MathMode::Inline,
                source_offset: node_offset(node, cx),
            },
        )
        .text(math.value.clone())
        .markdown(source),
    )
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
        return StyledText::new(node.as_text().to_string()).into_any_element();
    };
    let base_font_size = f32::from(text_style.font_size.to_pixels(window.rem_size()));
    let effective_font_size = match math.mode {
        MathMode::Inline => base_font_size * 0.83,
        MathMode::Display => base_font_size,
    };
    let key = MathKey {
        contents: math.contents.clone(),
        mode: math.mode,
        font_size: (effective_font_size * 100.0).round().max(1.0) as u32,
        font_family: text_style.font_family.clone(),
        color: color_bytes(text_style.color),
    };
    let render_key = RenderKey::Math(key.clone());
    let occurrence = RenderOccurrence {
        kind: RenderKind::Math,
        source_offset: math.source_offset,
    };
    let svg_renderer = cx.svg_renderer();
    let text_view = markdown
        .upgrade()
        .map(|markdown| markdown.read(cx).text_view.clone());
    let snapshot = text_view
        .and_then(|text_view| {
            markdown
                .update(cx, |markdown, cx| {
                    markdown.embedded.ensure_rendered(
                        render_key,
                        occurrence,
                        text_view,
                        move || render_math_image(&key, svg_renderer),
                        cx,
                    )
                })
                .ok()
        })
        .unwrap_or(RenderSnapshot::Source);

    let content = match snapshot {
        RenderSnapshot::Image(image) => img(ImageSource::Render(image))
            .object_fit(ObjectFit::Contain)
            .flex_none()
            .into_any_element(),
        RenderSnapshot::Source => StyledText::new(math.source.clone()).into_any_element(),
    };

    match math.mode {
        MathMode::Inline => content,
        MathMode::Display => div()
            .flex()
            .w_full()
            .items_center()
            .justify_center()
            .py_2()
            .child(content)
            .into_any_element(),
    }
}

fn render_math_image(
    key: &MathKey,
    svg_renderer: gpui::SvgRenderer,
) -> Result<Arc<RenderImage>, SharedString> {
    let svg = render_math_svg(key)?;
    svg_renderer
        .render_single_frame(svg.as_bytes(), 1.0)
        .map_err(|error| SharedString::from(format!("failed to render math SVG: {error}")))
}

fn render_math_svg(key: &MathKey) -> Result<String, SharedString> {
    let ast = ratex_parser::parse(key.contents.as_ref())
        .map_err(|error| SharedString::from(format!("failed to parse math: {error}")))?;
    let style = match key.mode {
        MathMode::Inline => MathStyle::Text,
        MathMode::Display => MathStyle::Display,
    };
    let options = LayoutOptions::default()
        .with_style(style)
        .with_color(ratex_color(key.color));
    let layout_box = layout(&ast, &options);
    let display_list = to_display_list(&layout_box);
    Ok(render_to_svg(
        &display_list,
        &SvgOptions {
            font_size: key.font_size as f64 / 100.0,
            padding: match key.mode {
                MathMode::Inline => 0.5,
                MathMode::Display => 3.0,
            },
            stroke_width: 1.0,
            embed_glyphs: true,
            ..SvgOptions::default()
        },
    ))
}

fn parse_mermaid(node: &markdown_ast::Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
    let markdown_ast::Node::Code(code) = node else {
        return None;
    };
    let scale = parse_mermaid_info(code.lang.as_deref()?)?;
    let source = cx.node_source(node)?;
    if !is_closed_fence(source) || !is_supported_mermaid(&code.value) {
        return None;
    }

    Some(
        MarkdownNode::new(
            MERMAID_NODE_NAME,
            MermaidNode {
                contents: code.value.clone().into(),
                scale,
                source_offset: node_offset(node, cx),
            },
        )
        .text(code.value.clone())
        .markdown(source),
    )
}

fn parse_mermaid_info(info: &str) -> Option<u32> {
    let mut parts = info.split_whitespace();
    if !parts.next()?.eq_ignore_ascii_case("mermaid") {
        return None;
    }
    Some(
        parts
            .next()
            .map(|scale| scale.trim_end_matches('%'))
            .and_then(|scale| scale.parse().ok())
            .unwrap_or(100)
            .clamp(10, 500),
    )
}

fn is_closed_fence(source: &str) -> bool {
    let mut lines = source.lines();
    let Some(first) = lines.next().map(str::trim_start) else {
        return false;
    };
    let Some(marker) = first
        .chars()
        .next()
        .filter(|marker| matches!(marker, '`' | '~'))
    else {
        return false;
    };
    let marker_len = first
        .chars()
        .take_while(|character| *character == marker)
        .count();
    if marker_len < 3 {
        return false;
    }
    let Some(last) = source.lines().next_back().map(str::trim) else {
        return false;
    };
    last.chars()
        .take_while(|character| *character == marker)
        .count()
        >= marker_len
        && last
            .chars()
            .all(|character| character == marker || character.is_whitespace())
}

fn is_supported_mermaid(source: &str) -> bool {
    const SUPPORTED: &[&str] = &[
        "flowchart",
        "graph",
        "sequenceDiagram",
        "classDiagram",
        "stateDiagram",
        "stateDiagram-v2",
        "erDiagram",
        "gantt",
        "pie",
        "gitGraph",
        "mindmap",
        "timeline",
        "quadrantChart",
        "xychart-beta",
        "journey",
    ];
    let first = source.split_whitespace().next().unwrap_or_default();
    SUPPORTED
        .iter()
        .any(|supported| first.eq_ignore_ascii_case(supported))
}

fn render_mermaid_node(
    markdown: WeakEntity<Markdown>,
    node: &MarkdownNode,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(mermaid) = node.data::<MermaidNode>() else {
        return StyledText::new(node.as_text().to_string()).into_any_element();
    };
    let showing_code = markdown
        .upgrade()
        .map(|markdown| {
            markdown
                .read(cx)
                .embedded
                .mermaid_code_tabs
                .contains(&mermaid.source_offset)
        })
        .unwrap_or(false);
    let text_style = window.text_style();

    let body = if showing_code {
        div()
            .w_full()
            .min_w_0()
            .p_3()
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted)
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .child(StyledText::new(mermaid.contents.clone()))
            .into_any_element()
    } else {
        render_mermaid_preview(markdown.clone(), mermaid, &text_style, cx)
    };

    let preview_markdown = markdown.clone();
    let code_markdown = markdown;
    let offset = mermaid.source_offset;
    v_flex()
        .id(("mermaid", offset))
        .w_full()
        .min_w_0()
        .gap_2()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new(("mermaid-preview", offset))
                                .label("Preview")
                                .xsmall()
                                .ghost()
                                .selected(!showing_code)
                                .on_click(move |_, _, cx| {
                                    set_mermaid_code_tab(&preview_markdown, offset, false, cx);
                                }),
                        )
                        .child(
                            Button::new(("mermaid-code", offset))
                                .label("Code")
                                .xsmall()
                                .ghost()
                                .selected(showing_code)
                                .on_click(move |_, _, cx| {
                                    set_mermaid_code_tab(&code_markdown, offset, true, cx);
                                }),
                        ),
                )
                .child(
                    Clipboard::new(("mermaid-copy", offset))
                        .value(mermaid.contents.clone())
                        .tooltip("Copy Mermaid source"),
                ),
        )
        .child(
            div()
                .id(("mermaid-scroll", offset))
                .w_full()
                .min_w_0()
                .overflow_x_scroll()
                .child(body),
        )
        .into_any_element()
}

fn set_mermaid_code_tab(
    markdown: &WeakEntity<Markdown>,
    source_offset: usize,
    showing_code: bool,
    cx: &mut App,
) {
    let _ = markdown.update(cx, |markdown, cx| {
        if showing_code {
            markdown.embedded.mermaid_code_tabs.insert(source_offset);
        } else {
            markdown.embedded.mermaid_code_tabs.remove(&source_offset);
        }
        markdown.text_view.update(cx, |_, cx| cx.notify());
        cx.notify();
    });
}

fn render_mermaid_preview(
    markdown: WeakEntity<Markdown>,
    mermaid: &MermaidNode,
    text_style: &TextStyle,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.theme().colors;
    let theme = mermaid_theme(colors, text_style.font_family.to_string());
    let key = MermaidKey {
        contents: mermaid.contents.clone(),
        scale: mermaid.scale,
        font_family: text_style.font_family.clone(),
        theme: mermaid_theme_key(colors),
    };
    let render_key = RenderKey::Mermaid(key.clone());
    let occurrence = RenderOccurrence {
        kind: RenderKind::Mermaid,
        source_offset: mermaid.source_offset,
    };
    let svg_renderer = cx.svg_renderer();
    let text_view = markdown
        .upgrade()
        .map(|markdown| markdown.read(cx).text_view.clone());
    let snapshot = text_view
        .and_then(|text_view| {
            markdown
                .update(cx, |markdown, cx| {
                    markdown.embedded.ensure_rendered(
                        render_key,
                        occurrence,
                        text_view,
                        move || {
                            let svg = glyphst_mermaid_render::render_to_svg(&key.contents, &theme)
                                .map_err(|error| {
                                    SharedString::from(format!(
                                        "failed to render Mermaid: {error:#}"
                                    ))
                                })?;
                            svg_renderer
                                .render_single_frame(svg.as_bytes(), key.scale as f32 / 100.0)
                                .map_err(|error| {
                                    SharedString::from(format!(
                                        "failed to render Mermaid SVG: {error}"
                                    ))
                                })
                        },
                        cx,
                    )
                })
                .ok()
        })
        .unwrap_or(RenderSnapshot::Source);

    match snapshot {
        RenderSnapshot::Image(image) => div()
            .w_full()
            .min_w_0()
            .flex()
            .justify_center()
            .child(
                img(ImageSource::Render(image))
                    .object_fit(ObjectFit::Contain)
                    .max_w_full(),
            )
            .into_any_element(),
        RenderSnapshot::Source => div()
            .w_full()
            .min_w_0()
            .p_3()
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted)
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .child(StyledText::new(mermaid.contents.clone()))
            .into_any_element(),
    }
}

fn color_bytes(color: Hsla) -> [u8; 4] {
    let rgba: Rgba = color.to_rgb();
    [
        (rgba.r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgba.g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgba.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgba.a.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn ratex_color(color: [u8; 4]) -> Color {
    Color::new(
        color[0] as f32 / 255.0,
        color[1] as f32 / 255.0,
        color[2] as f32 / 255.0,
        color[3] as f32 / 255.0,
    )
}

fn mermaid_theme_key(colors: ThemeColor) -> [[u8; 4]; 16] {
    [
        color_bytes(colors.background),
        color_bytes(colors.foreground),
        color_bytes(colors.muted),
        color_bytes(colors.border),
        color_bytes(colors.secondary),
        color_bytes(colors.accent),
        color_bytes(colors.warning),
        color_bytes(colors.danger),
        color_bytes(colors.chart_1),
        color_bytes(colors.chart_2),
        color_bytes(colors.chart_3),
        color_bytes(colors.chart_4),
        color_bytes(colors.chart_5),
        color_bytes(colors.success),
        color_bytes(colors.button),
        color_bytes(colors.warning_active),
    ]
}

fn mermaid_theme(colors: ThemeColor, font_family: String) -> MermaidTheme {
    let git_branch_colors = [
        colors.chart_1,
        colors.chart_2,
        colors.chart_3,
        colors.chart_4,
        colors.chart_5,
        colors.success,
        colors.warning,
        colors.danger,
    ];
    let git_branch_label_colors =
        git_branch_colors.map(glyphst_mermaid_render::text_color_for_background);
    let accents = [
        colors.chart_1,
        colors.chart_2,
        colors.chart_3,
        colors.chart_4,
        colors.chart_5,
    ];

    MermaidTheme {
        dark_mode: colors.background.l < 0.5,
        font_family,
        background: colors.background,
        primary_color: colors.muted,
        primary_text_color: colors.foreground,
        primary_border_color: colors.border,
        secondary_color: colors.secondary,
        tertiary_color: colors.background,
        line_color: colors.border,
        text_color: colors.foreground,
        edge_label_background: colors.background,
        cluster_background: colors.muted,
        cluster_border: colors.border,
        note_background: colors.warning,
        note_border: colors.warning_active,
        actor_background: colors.button,
        actor_border: colors.border,
        activation_background: colors.accent,
        activation_border: colors.border,
        git_branch_colors,
        git_branch_label_colors,
        er_attr_bg_odd: colors.muted,
        er_attr_bg_even: colors.background,
        error_color: colors.danger,
        warning_color: colors.warning,
        accent_colors: accents
            .into_iter()
            .map(|background| AccentColor {
                foreground: glyphst_mermaid_render::text_color_for_background(background),
                background,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mermaid_info_parses_and_clamps_scale() {
        assert_eq!(parse_mermaid_info("mermaid"), Some(100));
        assert_eq!(parse_mermaid_info("mermaid 150"), Some(150));
        assert_eq!(parse_mermaid_info("mermaid 250%"), Some(250));
        assert_eq!(parse_mermaid_info("mermaid 2"), Some(10));
        assert_eq!(parse_mermaid_info("mermaid 900"), Some(500));
        assert_eq!(parse_mermaid_info("rust"), None);
    }

    #[test]
    fn mermaid_requires_closed_supported_fence() {
        assert!(is_closed_fence("```mermaid\ngraph TD\n```"));
        assert!(is_closed_fence("~~~mermaid\ngraph TD\n~~~"));
        assert!(!is_closed_fence("```mermaid\ngraph TD"));
        assert!(is_supported_mermaid("flowchart TD\nA --> B"));
        assert!(!is_supported_mermaid("packet-beta\n0-7: Header"));
    }

    #[test]
    fn ratex_renders_inline_and_display_svg() {
        for mode in [MathMode::Inline, MathMode::Display] {
            let svg = render_math_svg(&MathKey {
                contents: r"\frac{x^2}{y_1}".into(),
                mode,
                font_size: 1_400,
                font_family: "Inter".into(),
                color: [32, 48, 64, 255],
            })
            .expect("math should render");
            assert!(svg.contains("<svg"));
            assert!(svg.contains("<path"));
        }
    }
}
