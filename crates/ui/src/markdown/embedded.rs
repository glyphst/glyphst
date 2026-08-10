use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};

use super::{Markdown, MarkdownFeatures};
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

const MERMAID_NODE_NAME: &str = "glyphst-mermaid";

#[derive(Clone, Debug)]
struct MermaidNode {
    contents: SharedString,
    scale: u32,
    source_offset: usize,
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
    Mermaid(MermaidKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RenderKind {
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
        extensions = super::katex::add_extensions(extensions, markdown.clone());
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
}
