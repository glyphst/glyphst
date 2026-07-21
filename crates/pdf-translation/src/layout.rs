use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{Block, BlockRole, BoundingBox, PageGeometry, TranslatedDocument};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutDocument {
    pub pages: Vec<LayoutPage>,
    pub warnings: Vec<LayoutWarning>,
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
    pub bbox: BoundingBox,
    pub text: String,
    pub role: BlockRole,
    pub font_size_pt: f32,
    pub minimum_font_size_pt: f32,
    pub leading_em: f32,
    pub minimum_leading_em: f32,
    pub font_weight: FontWeight,
    pub first_line_indent_pt: f32,
    pub justify: bool,
    pub preserve_line_breaks: bool,
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
        for (index, (block, text)) in member_blocks.into_iter().zip(text_parts).enumerate() {
            if text.trim().is_empty() {
                continue;
            }
            let body_font = body_font_by_page[block.page_index];
            let font_size = role_base_font(block.role, body_font)
                .min((block.bbox.height() * role_height_ratio(block.role)).max(4.2));
            let minimum_font_size = minimum_font_size(block.role, font_size);
            let leading = role_leading(block.role);
            let item = LayoutItem {
                unit_id: unit.id.clone(),
                block_id: block.id.clone(),
                page_index: block.page_index,
                bbox: block.bbox,
                text,
                role: block.role,
                font_size_pt: round(font_size),
                minimum_font_size_pt: round(minimum_font_size),
                leading_em: leading,
                minimum_leading_em: (leading * 0.78).max(0.20),
                font_weight: if matches!(block.role, BlockRole::Title | BlockRole::Heading) {
                    FontWeight::Bold
                } else {
                    FontWeight::Regular
                },
                first_line_indent_pt: if block.role == BlockRole::Body && index == 0 {
                    round((font_size * 2.0).min(block.bbox.width() * 0.08))
                } else {
                    0.0
                },
                justify: block.role == BlockRole::Body
                    && !contains_long_unbreakable(&unit.translated_text),
                preserve_line_breaks: matches!(block.role, BlockRole::TableOfContents),
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
        detect_collisions(page, &mut warnings);
    }
    LayoutDocument { pages, warnings }
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
        .filter(|block| block.role == BlockRole::Body)
        .filter_map(|block| {
            block
                .metadata
                .get("average_font_size")
                .and_then(|value| value.as_f64())
                .map(|value| value as f32)
                .filter(|value| value.is_finite() && *value >= 5.0 && *value <= 36.0)
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        return 10.0;
    }
    values.sort_by(f32::total_cmp);
    values[values.len() / 2].clamp(8.0, 13.0)
}

fn role_base_font(role: BlockRole, body_font: f32) -> f32 {
    match role {
        BlockRole::Title => (body_font * 1.85).clamp(15.0, 24.0),
        BlockRole::Heading => (body_font * 1.28).clamp(11.0, 17.0),
        BlockRole::Abstract | BlockRole::Body => body_font,
        BlockRole::TableOfContents => (body_font * 0.92).clamp(8.0, 11.0),
        BlockRole::FigureCaption | BlockRole::TableCaption => (body_font * 0.86).clamp(7.5, 10.5),
        BlockRole::Footnote => (body_font * 0.78).clamp(6.8, 9.0),
        _ => body_font,
    }
}

fn minimum_font_size(role: BlockRole, font_size: f32) -> f32 {
    let ratio = match role {
        BlockRole::Title => 0.58,
        BlockRole::Heading => 0.62,
        BlockRole::FigureCaption | BlockRole::TableCaption | BlockRole::Footnote => 0.55,
        _ => 0.65,
    };
    (font_size * ratio).max(4.2).min(font_size)
}

fn role_height_ratio(role: BlockRole) -> f32 {
    match role {
        BlockRole::Title | BlockRole::Heading => 0.82,
        BlockRole::FigureCaption | BlockRole::TableCaption | BlockRole::Footnote => 0.68,
        _ => 0.72,
    }
}

fn role_leading(role: BlockRole) -> f32 {
    match role {
        BlockRole::Title | BlockRole::Heading => 0.62,
        BlockRole::FigureCaption | BlockRole::TableCaption | BlockRole::Footnote => 0.48,
        _ => 0.58,
    }
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
