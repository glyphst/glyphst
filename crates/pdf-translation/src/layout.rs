use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    Block, BlockRole, BoundingBox, FontFamilyClass, PageGeometry, StyledRun, TextStyle,
    TranslatedDocument,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutDocument {
    pub pages: Vec<LayoutPage>,
    pub warnings: Vec<LayoutWarning>,
    #[serde(default)]
    pub preserved_on_overflow: Vec<String>,
    #[serde(default)]
    pub overflow_policy: OverflowPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutPage {
    pub page_index: usize,
    pub geometry: PageGeometry,
    pub items: Vec<LayoutItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutItem {
    pub unit_id: String,
    pub block_id: String,
    pub page_index: usize,
    pub source_bbox: BoundingBox,
    #[serde(default)]
    pub source_regions: Vec<BoundingBox>,
    pub bbox: BoundingBox,
    pub text: String,
    pub role: BlockRole,
    pub source_font_size_pt: f32,
    pub font_size_pt: f32,
    pub minimum_font_size_pt: f32,
    pub emergency_minimum_font_size_pt: f32,
    pub leading_em: f32,
    pub minimum_leading_em: f32,
    pub font_weight: FontWeight,
    pub font_class: FontFamilyClass,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub color_argb: u32,
    #[serde(default)]
    pub styled_runs: Vec<StyledRun>,
    pub first_line_indent_pt: f32,
    pub justify: bool,
    pub preserve_line_breaks: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverflowPolicy {
    #[default]
    Strict,
    Adaptive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutOptions {
    pub overflow_policy: OverflowPolicy,
}

impl LayoutOptions {
    pub const fn strict() -> Self {
        Self {
            overflow_policy: OverflowPolicy::Strict,
        }
    }

    pub const fn application_default() -> Self {
        Self {
            overflow_policy: OverflowPolicy::Adaptive,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontWeight {
    Regular,
    Bold,
}

impl FontWeight {
    pub fn typst_name(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Bold => "bold",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutWarning {
    pub unit_id: String,
    pub message: String,
}

pub fn build_layout(translated: &TranslatedDocument) -> LayoutDocument {
    build_layout_with_options(translated, LayoutOptions::strict())
}

pub fn build_layout_with_options(
    translated: &TranslatedDocument,
    options: LayoutOptions,
) -> LayoutDocument {
    let blocks = translated
        .document
        .pages
        .iter()
        .flat_map(|page| page.blocks.iter())
        .map(|block| (block.id.as_str(), block))
        .collect::<HashMap<_, _>>();
    let body_font_by_page = translated
        .document
        .pages
        .iter()
        .map(|page| estimate_body_font_size(&page.blocks))
        .collect::<Vec<_>>();
    let mut pages = translated
        .document
        .pages
        .iter()
        .map(|page| LayoutPage {
            page_index: page.page_index,
            geometry: page.geometry,
            items: Vec::new(),
        })
        .collect::<Vec<_>>();
    let mut warnings = Vec::new();

    for unit in &translated.units {
        let member_blocks = unit
            .member_block_ids
            .iter()
            .filter_map(|id| blocks.get(id.as_str()).copied())
            .collect::<Vec<_>>();
        if member_blocks.is_empty() {
            warnings.push(LayoutWarning {
                unit_id: unit.id.clone(),
                message: "translation unit has no source layout boxes".to_string(),
            });
            continue;
        }
        let capacities = member_blocks
            .iter()
            .map(|block| {
                text_capacity(
                    block.bbox,
                    role_base_font(block.role, body_font_by_page[block.page_index]),
                )
            })
            .collect::<Vec<_>>();
        let text_parts = split_text_for_layout_boxes(&unit.translated_text, &capacities);
        if text_parts.len() != member_blocks.len() {
            warnings.push(LayoutWarning {
                unit_id: unit.id.clone(),
                message: "translation could not be distributed across continuation boxes"
                    .to_string(),
            });
            continue;
        }
        let member_count = member_blocks.len();
        for (index, (block, text)) in member_blocks.into_iter().zip(text_parts).enumerate() {
            if text.trim().is_empty() {
                continue;
            }
            let body_font = body_font_by_page[block.page_index];
            let profile = source_typography(block, body_font);
            let font_size = profile.font_size_pt;
            let minimum_font_size = preferred_minimum_font_size(block.role, font_size);
            let emergency_minimum_font_size = emergency_minimum_font_size(block.role, font_size);
            let item = LayoutItem {
                unit_id: unit.id.clone(),
                block_id: block.id.clone(),
                page_index: block.page_index,
                source_bbox: block.bbox,
                source_regions: source_text_regions(block),
                bbox: block.bbox,
                text,
                role: block.role,
                source_font_size_pt: font_size,
                font_size_pt: round(font_size),
                minimum_font_size_pt: round(minimum_font_size),
                emergency_minimum_font_size_pt: round(emergency_minimum_font_size),
                leading_em: profile.leading_em,
                minimum_leading_em: (profile.leading_em * 0.78).max(0.20),
                font_weight: if profile.style.bold {
                    FontWeight::Bold
                } else {
                    FontWeight::Regular
                },
                font_class: profile.style.font_class.unwrap_or(FontFamilyClass::Serif),
                italic: profile.style.italic,
                underline: profile.style.underline,
                strikeout: profile.style.strikeout,
                color_argb: profile.style.color_argb.unwrap_or(0xff00_0000),
                styled_runs: if member_count == 1 {
                    unit.styled_runs.clone()
                } else {
                    Vec::new()
                },
                first_line_indent_pt: if index == 0 {
                    profile.first_line_indent_pt
                } else {
                    0.0
                },
                justify: profile.justify && !contains_long_unbreakable(&unit.translated_text),
                preserve_line_breaks: profile.preserve_line_breaks,
            };
            if let Some(page) = pages.get_mut(block.page_index) {
                page.items.push(item);
            }
        }
    }
    for page in &mut pages {
        page.items.sort_by(|left, right| {
            left.bbox
                .y0
                .total_cmp(&right.bbox.y0)
                .then(left.bbox.x0.total_cmp(&right.bbox.x0))
        });
    }
    expand_into_safe_vertical_space(&mut pages, &translated.document);
    for page in &pages {
        detect_collisions(page, &mut warnings);
    }
    LayoutDocument {
        pages,
        warnings,
        preserved_on_overflow: Vec::new(),
        overflow_policy: options.overflow_policy,
    }
}

fn source_text_regions(block: &Block) -> Vec<BoundingBox> {
    let mut regions = block
        .content
        .lines
        .iter()
        .filter_map(|line| {
            line.segments
                .iter()
                .filter(|segment| !segment.text.trim().is_empty())
                .map(|segment| segment.bbox)
                .reduce(BoundingBox::union)
                .or_else(|| (line.bbox.area() > 0.0).then_some(line.bbox))
        })
        .map(|bbox| {
            let vertical_padding = (bbox.height() * 0.08).clamp(0.25, 0.8);
            BoundingBox::new(
                bbox.x0 - 0.35,
                bbox.y0 - vertical_padding,
                bbox.x1 + 0.35,
                bbox.y1 + vertical_padding,
            )
            .clamp_to(block.bbox)
        })
        .filter(|bbox| bbox.area() > 0.0)
        .collect::<Vec<_>>();
    if regions.is_empty() {
        regions.push(block.bbox);
    }
    regions
}

pub fn split_text_for_layout_boxes(text: &str, capacities: &[usize]) -> Vec<String> {
    if capacities.is_empty() {
        return Vec::new();
    }
    if capacities.len() == 1 {
        return vec![text.trim().to_string()];
    }
    let words = text.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() {
        return vec![String::new(); capacities.len()];
    }
    let total_capacity = capacities.iter().sum::<usize>().max(capacities.len());
    let total_characters = words.iter().map(|word| word.chars().count()).sum::<usize>()
        + words.len().saturating_sub(1);
    let targets = capacities
        .iter()
        .map(|capacity| {
            ((total_characters as f32 * *capacity as f32 / total_capacity as f32).round() as usize)
                .max(1)
        })
        .collect::<Vec<_>>();
    let mut parts = Vec::with_capacity(capacities.len());
    let mut current = String::new();
    let mut box_index = 0;
    for word in words {
        let projected =
            current.chars().count() + usize::from(!current.is_empty()) + word.chars().count();
        if box_index + 1 < capacities.len() && !current.is_empty() && projected > targets[box_index]
        {
            parts.push(std::mem::take(&mut current));
            box_index += 1;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    parts.push(current);
    while parts.len() < capacities.len() {
        parts.push(String::new());
    }
    parts
}

fn estimate_body_font_size(blocks: &[Block]) -> f32 {
    let mut values = blocks
        .iter()
        .filter(|block| {
            matches!(
                block.role,
                BlockRole::Abstract
                    | BlockRole::Body
                    | BlockRole::FigureCaption
                    | BlockRole::TableCaption
                    | BlockRole::Footnote
            )
        })
        .filter_map(|block| {
            block
                .metadata
                .get("average_font_size")
                .and_then(|value| value.as_f64())
                .map(|value| value as f32)
                .filter(|value| value.is_finite() && *value >= 5.0 && *value <= 36.0)
                .map(|value| {
                    (
                        value,
                        block
                            .content
                            .text
                            .chars()
                            .filter(|character| !character.is_whitespace())
                            .count()
                            .max(1),
                    )
                })
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        return 10.0;
    }
    values.sort_by(|left, right| left.0.total_cmp(&right.0));
    let midpoint = values
        .iter()
        .map(|(_, weight)| *weight)
        .sum::<usize>()
        .div_ceil(2);
    let mut cumulative = 0usize;
    for (value, weight) in values {
        cumulative += weight;
        if cumulative >= midpoint {
            return value.clamp(5.0, 13.0);
        }
    }
    10.0
}

fn role_base_font(role: BlockRole, body_font: f32) -> f32 {
    match role {
        BlockRole::Title => (body_font * 1.85).clamp(15.0, 24.0),
        BlockRole::Heading => (body_font * 1.28).clamp(11.0, 17.0),
        BlockRole::Abstract | BlockRole::Body | BlockRole::Prompt | BlockRole::Dialogue => {
            body_font
        }
        BlockRole::TableOfContents => (body_font * 0.92).clamp(8.0, 11.0),
        BlockRole::FigureCaption | BlockRole::TableCaption => (body_font * 0.86).clamp(7.5, 10.5),
        BlockRole::Footnote => (body_font * 0.78).clamp(6.8, 9.0),
        _ => body_font,
    }
}

fn preferred_minimum_font_size(role: BlockRole, font_size: f32) -> f32 {
    let absolute_floor = match role {
        BlockRole::Title => 10.0,
        BlockRole::Heading => 7.0,
        BlockRole::Footnote => 5.0,
        BlockRole::FigureCaption | BlockRole::TableCaption => 5.5,
        _ => 6.5,
    };
    font_size.min((font_size * 0.88).max(absolute_floor))
}

fn emergency_minimum_font_size(role: BlockRole, font_size: f32) -> f32 {
    let absolute_floor = match role {
        BlockRole::Title => 8.0,
        BlockRole::Heading => 6.0,
        BlockRole::Footnote => 4.8,
        BlockRole::FigureCaption | BlockRole::TableCaption => 5.0,
        _ => 5.5,
    };
    font_size.min((font_size * 0.70).max(absolute_floor))
}

fn role_leading(role: BlockRole) -> f32 {
    match role {
        BlockRole::Title | BlockRole::Heading => 0.62,
        BlockRole::FigureCaption | BlockRole::TableCaption | BlockRole::Footnote => 0.48,
        _ => 0.58,
    }
}

struct SourceTypography {
    style: TextStyle,
    font_size_pt: f32,
    leading_em: f32,
    first_line_indent_pt: f32,
    justify: bool,
    preserve_line_breaks: bool,
}

fn source_typography(block: &Block, body_font: f32) -> SourceTypography {
    let mut styles = Vec::<(TextStyle, usize)>::new();
    for segment in block
        .content
        .lines
        .iter()
        .flat_map(|line| line.segments.iter())
    {
        let weight = segment
            .text
            .chars()
            .filter(|character| !character.is_whitespace())
            .count();
        if weight == 0 {
            continue;
        }
        if let Some((_, total)) = styles
            .iter_mut()
            .find(|(style, _)| typography_styles_match(style, &segment.style))
        {
            *total += weight;
        } else {
            styles.push((segment.style.clone(), weight));
        }
    }
    let mut style = styles
        .into_iter()
        .max_by_key(|(_, weight)| *weight)
        .map(|(style, _)| style)
        .unwrap_or_default();
    let font_size = style
        .font_size
        .or_else(|| {
            block
                .metadata
                .get("average_font_size")
                .and_then(|value| value.as_f64())
                .map(|value| value as f32)
        })
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or_else(|| role_base_font(block.role, body_font))
        .clamp(4.5, 48.0);
    style.font_size = Some(font_size);
    if style.font_class.is_none() {
        style.font_class = Some(FontFamilyClass::Serif);
    }
    if matches!(block.role, BlockRole::Title | BlockRole::Heading)
        && block
            .metadata
            .get("bold_ratio")
            .and_then(|value| value.as_f64())
            .is_some_and(|ratio| ratio >= 0.55)
    {
        style.bold = true;
    }
    let leading_em = block
        .metadata
        .get("line_pitch_pt")
        .and_then(|value| value.as_f64())
        .map(|value| value as f32)
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|pitch| (pitch / font_size - 0.78).clamp(0.20, 1.05))
        .unwrap_or_else(|| role_leading(block.role));
    let first_line_indent_pt = block
        .metadata
        .get("first_line_indent_pt")
        .and_then(|value| value.as_f64())
        .map(|value| value as f32)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(0.0)
        .min(block.bbox.width() * 0.20);
    let justify = block
        .metadata
        .get("justify")
        .and_then(|value| value.as_bool())
        .unwrap_or(matches!(block.role, BlockRole::Body | BlockRole::Prompt));
    SourceTypography {
        style,
        font_size_pt: round(font_size),
        leading_em: round(leading_em),
        first_line_indent_pt: round(first_line_indent_pt),
        justify,
        preserve_line_breaks: matches!(
            block.role,
            BlockRole::Prompt | BlockRole::Dialogue | BlockRole::TableOfContents | BlockRole::Code
        ) || looks_like_dialogue(&block.content.text),
    }
}

fn typography_styles_match(left: &TextStyle, right: &TextStyle) -> bool {
    left.font_class == right.font_class
        && left.baseline_position == right.baseline_position
        && left.bold == right.bold
        && left.italic == right.italic
        && left.underline == right.underline
        && left.strikeout == right.strikeout
        && left.color_argb == right.color_argb
        && match (left.font_size, right.font_size) {
            (Some(left), Some(right)) => (left - right).abs() <= 0.5,
            (None, None) => true,
            _ => false,
        }
}

fn looks_like_dialogue(text: &str) -> bool {
    let labels = text
        .lines()
        .filter_map(|line| {
            let (speaker, text) = line.split_once([':', '：'])?;
            let speaker = speaker.trim();
            let words = speaker.split_whitespace().collect::<Vec<_>>();
            (!text.trim().is_empty()
                && speaker.chars().count() <= 24
                && (1..=3).contains(&words.len())
                && words.iter().all(|word| {
                    word.chars().all(|character| {
                        character.is_alphabetic() || matches!(character, '-' | '\'' | '’')
                    })
                }))
            .then(|| speaker.to_lowercase())
        })
        .collect::<Vec<_>>();
    labels.len() >= 3
        && labels
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            >= 2
}

fn expand_into_safe_vertical_space(pages: &mut [LayoutPage], document: &crate::DocumentV1) {
    for page in pages {
        let Some(source_page) = document.pages.get(page.page_index) else {
            continue;
        };
        for item in &mut page.items {
            let nearest = source_page
                .blocks
                .iter()
                .filter(|block| block.id != item.block_id && block.bbox.y0 >= item.source_bbox.y1)
                .filter(|block| horizontal_overlap(item.source_bbox, block.bbox) >= 0.25)
                .map(|block| block.bbox.y0)
                .min_by(f32::total_cmp)
                .unwrap_or(page.geometry.height - 2.0);
            let growth_cap = (item.source_bbox.height() * 0.35).clamp(0.0, 24.0);
            let safe_y1 = (nearest - 1.5)
                .min(item.source_bbox.y1 + growth_cap)
                .min(page.geometry.height - 2.0);
            if safe_y1 > item.bbox.y1 + 1.0 {
                item.bbox.y1 = safe_y1;
            }
        }
    }
}

fn horizontal_overlap(left: BoundingBox, right: BoundingBox) -> f32 {
    let overlap = (left.x1.min(right.x1) - left.x0.max(right.x0)).max(0.0);
    overlap / left.width().min(right.width()).max(1.0)
}

fn text_capacity(bbox: BoundingBox, font_size: f32) -> usize {
    let characters_per_line = (bbox.width() / (font_size * 0.92)).floor().max(4.0);
    let lines = (bbox.height() / (font_size * 1.12)).floor().max(1.0);
    (characters_per_line * lines) as usize
}

fn contains_long_unbreakable(text: &str) -> bool {
    text.split_whitespace()
        .any(|word| word.chars().count() >= 36)
}

fn detect_collisions(page: &LayoutPage, warnings: &mut Vec<LayoutWarning>) {
    for (index, item) in page.items.iter().enumerate() {
        for other in page.items.iter().skip(index + 1) {
            let Some(intersection) = item.bbox.intersection(other.bbox) else {
                continue;
            };
            let minimum_area = item.bbox.area().min(other.bbox.area()).max(1.0);
            if intersection.area() / minimum_area > 0.12 {
                warnings.push(LayoutWarning {
                    unit_id: item.unit_id.clone(),
                    message: format!(
                        "translated layout box overlaps {} on page {}",
                        other.block_id,
                        page.page_index + 1
                    ),
                });
            }
        }
    }
}

fn round(value: f32) -> f32 {
    (value * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_text_is_distributed_across_boxes() {
        let parts = split_text_for_layout_boxes(
            "one two three four five six seven eight nine ten",
            &[20, 30],
        );

        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts.join(" "),
            "one two three four five six seven eight nine ten"
        );
        assert!(!parts[0].is_empty());
        assert!(!parts[1].is_empty());
    }
}
