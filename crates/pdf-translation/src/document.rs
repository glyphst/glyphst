use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DOCUMENT_SCHEMA: &str = "glyphst.document";
pub const DOCUMENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentV1 {
    pub schema: String,
    pub schema_version: u32,
    pub metadata: DocumentMetadata,
    pub pages: Vec<Page>,
}

impl DocumentV1 {
    pub fn new(metadata: DocumentMetadata, pages: Vec<Page>) -> Self {
        Self {
            schema: DOCUMENT_SCHEMA.to_string(),
            schema_version: DOCUMENT_SCHEMA_VERSION,
            metadata,
            pages,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != DOCUMENT_SCHEMA {
            bail!("unsupported document schema {}", self.schema);
        }
        if self.schema_version != DOCUMENT_SCHEMA_VERSION {
            bail!(
                "unsupported document schema version {}",
                self.schema_version
            );
        }
        for (page_index, page) in self.pages.iter().enumerate() {
            if page.page_index != page_index {
                bail!(
                    "page index mismatch: expected {page_index}, found {}",
                    page.page_index
                );
            }
            if page.geometry.width <= 0.0 || page.geometry.height <= 0.0 {
                bail!("page {} has invalid dimensions", page_index + 1);
            }
            for block in &page.blocks {
                if block.page_index != page_index {
                    bail!("block {} is assigned to the wrong page", block.id);
                }
                if !block.bbox.is_valid() {
                    bail!("block {} has an invalid bounding box", block.id);
                }
            }
            for region in &page.protected_regions {
                if region.page_index != page_index {
                    bail!(
                        "protected region {} is assigned to the wrong page",
                        region.id
                    );
                }
                if !region.bbox.is_valid() {
                    bail!("protected region {} has an invalid bounding box", region.id);
                }
                if !region.confidence.is_finite() || !(0.0..=1.0).contains(&region.confidence) {
                    bail!("protected region {} has invalid confidence", region.id);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub document_id: String,
    pub source_path: PathBuf,
    pub provider: String,
    pub provider_version: String,
    #[serde(default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub page_index: usize,
    pub geometry: PageGeometry,
    pub blocks: Vec<Block>,
    #[serde(default)]
    pub protected_regions: Vec<ProtectedRegion>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PageGeometry {
    pub width: f32,
    pub height: f32,
    pub crop_box: BoundingBox,
    pub rotation_degrees: i32,
}

impl PageGeometry {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            crop_box: BoundingBox::new(0.0, 0.0, width, height),
            rotation_degrees: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub id: String,
    pub page_index: usize,
    pub order: usize,
    pub reading_order: usize,
    pub kind: BlockKind,
    pub role: BlockRole,
    pub bbox: BoundingBox,
    pub content: BlockContent,
    pub policy: TranslationPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ContinuationHint>,
    pub source: SourceProvenance,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Text,
    Image,
    Table,
    Formula,
    Code,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockRole {
    Title,
    Heading,
    Body,
    Abstract,
    TableOfContents,
    FigureCaption,
    TableCaption,
    Footnote,
    Header,
    Footer,
    PageNumber,
    Metadata,
    Reference,
    FormulaNumber,
    Image,
    Table,
    Formula,
    Code,
    Unknown,
}

impl BlockRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Heading => "heading",
            Self::Body => "body",
            Self::Abstract => "abstract",
            Self::TableOfContents => "table_of_contents",
            Self::FigureCaption => "figure_caption",
            Self::TableCaption => "table_caption",
            Self::Footnote => "footnote",
            Self::Header => "header",
            Self::Footer => "footer",
            Self::PageNumber => "page_number",
            Self::Metadata => "metadata",
            Self::Reference => "reference",
            Self::FormulaNumber => "formula_number",
            Self::Image => "image",
            Self::Table => "table",
            Self::Formula => "formula",
            Self::Code => "code",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BlockContent {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub lines: Vec<Line>,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub bbox: BoundingBox,
    pub text: String,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub kind: SegmentKind,
    pub text: String,
    pub bbox: BoundingBox,
    #[serde(default)]
    pub style: TextStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    Text,
    Formula,
    Citation,
    Url,
    Literal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TextStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_class: Option<FontFamilyClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub strikeout: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_argb: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontFamilyClass {
    Serif,
    SansSerif,
    Monospace,
    Math,
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecisionSource {
    #[default]
    Unknown,
    NativeHeuristic,
    OcrSemantic,
    ProtectedRegion,
    DeterministicRule,
    FusionConflict,
    AiReview,
    OverflowFallback,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranslationPolicy {
    pub translate: bool,
    pub reason: String,
    #[serde(default)]
    pub source: PolicyDecisionSource,
    #[serde(default = "default_policy_confidence")]
    pub confidence: f32,
    #[serde(default)]
    pub locked: bool,
}

impl TranslationPolicy {
    pub fn translate(reason: impl Into<String>) -> Self {
        Self {
            translate: true,
            reason: reason.into(),
            source: PolicyDecisionSource::DeterministicRule,
            confidence: 1.0,
            locked: false,
        }
    }

    pub fn preserve(reason: impl Into<String>) -> Self {
        Self {
            translate: false,
            reason: reason.into(),
            source: PolicyDecisionSource::DeterministicRule,
            confidence: 1.0,
            locked: false,
        }
    }

    pub fn candidate(
        translate: bool,
        reason: impl Into<String>,
        source: PolicyDecisionSource,
        confidence: f32,
    ) -> Self {
        Self {
            translate,
            reason: reason.into(),
            source,
            confidence: confidence.clamp(0.0, 1.0),
            locked: false,
        }
    }

    pub fn locked_preserve(
        reason: impl Into<String>,
        source: PolicyDecisionSource,
        confidence: f32,
    ) -> Self {
        Self {
            translate: false,
            reason: reason.into(),
            source,
            confidence: confidence.clamp(0.0, 1.0),
            locked: true,
        }
    }

    pub fn unresolved(reason: impl Into<String>) -> Self {
        Self::candidate(false, reason, PolicyDecisionSource::FusionConflict, 0.0)
    }
}

const fn default_policy_confidence() -> f32 {
    1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedRegionKind {
    Chart,
    Table,
    Image,
    Formula,
    Code,
    Drawing,
    Unknown,
}

impl ProtectedRegionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chart => "chart",
            Self::Table => "table",
            Self::Image => "image",
            Self::Formula => "formula",
            Self::Code => "code",
            Self::Drawing => "drawing",
            Self::Unknown => "unknown",
        }
    }

    pub const fn block_role(self) -> BlockRole {
        match self {
            Self::Table => BlockRole::Table,
            Self::Formula => BlockRole::Formula,
            Self::Code => BlockRole::Code,
            Self::Chart | Self::Image | Self::Drawing | Self::Unknown => BlockRole::Image,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtectedRegion {
    pub id: String,
    pub page_index: usize,
    pub kind: ProtectedRegionKind,
    pub bbox: BoundingBox,
    pub source: String,
    pub confidence: f32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinuationHint {
    pub group_id: String,
    pub scope: ContinuationScope,
    pub reading_order: Option<usize>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationScope {
    IntraPage,
    CrossPage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceProvenance {
    pub provider: String,
    pub raw_label: String,
    pub raw_bbox: BoundingBox,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct BoundingBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl BoundingBox {
    pub const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn from_slice(values: &[Value]) -> Option<Self> {
        if values.len() != 4 {
            return None;
        }
        Some(Self::new(
            values[0].as_f64()? as f32,
            values[1].as_f64()? as f32,
            values[2].as_f64()? as f32,
            values[3].as_f64()? as f32,
        ))
    }

    pub fn width(self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }

    pub fn height(self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }

    pub fn area(self) -> f32 {
        self.width() * self.height()
    }

    pub fn is_valid(self) -> bool {
        self.x0.is_finite()
            && self.y0.is_finite()
            && self.x1.is_finite()
            && self.y1.is_finite()
            && self.x1 >= self.x0
            && self.y1 >= self.y0
    }

    pub fn scale(self, scale_x: f32, scale_y: f32) -> Self {
        Self::new(
            self.x0 * scale_x,
            self.y0 * scale_y,
            self.x1 * scale_x,
            self.y1 * scale_y,
        )
    }

    pub fn clamp_to(self, bounds: Self) -> Self {
        Self::new(
            self.x0.clamp(bounds.x0, bounds.x1),
            self.y0.clamp(bounds.y0, bounds.y1),
            self.x1.clamp(bounds.x0, bounds.x1),
            self.y1.clamp(bounds.y0, bounds.y1),
        )
    }

    pub fn union(self, other: Self) -> Self {
        Self::new(
            self.x0.min(other.x0),
            self.y0.min(other.y0),
            self.x1.max(other.x1),
            self.y1.max(other.y1),
        )
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let intersection = Self::new(
            self.x0.max(other.x0),
            self.y0.max(other.y0),
            self.x1.min(other.x1),
            self.y1.min(other.y1),
        );
        (intersection.x1 > intersection.x0 && intersection.y1 > intersection.y0)
            .then_some(intersection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blocks_on_the_wrong_page() {
        let document = DocumentV1::new(
            DocumentMetadata {
                document_id: "document".to_string(),
                source_path: "source.pdf".into(),
                provider: "test".to_string(),
                provider_version: "1".to_string(),
                extra: BTreeMap::new(),
            },
            vec![Page {
                page_index: 0,
                geometry: PageGeometry::new(100.0, 100.0),
                blocks: vec![Block {
                    id: "bad".to_string(),
                    page_index: 1,
                    order: 0,
                    reading_order: 0,
                    kind: BlockKind::Text,
                    role: BlockRole::Body,
                    bbox: BoundingBox::new(0.0, 0.0, 10.0, 10.0),
                    content: BlockContent::default(),
                    policy: TranslationPolicy::translate("test"),
                    continuation: None,
                    source: SourceProvenance {
                        provider: "test".to_string(),
                        raw_label: "text".to_string(),
                        raw_bbox: BoundingBox::new(0.0, 0.0, 10.0, 10.0),
                        raw_path: None,
                    },
                    tags: Vec::new(),
                    metadata: BTreeMap::new(),
                }],
                protected_regions: Vec::new(),
                metadata: BTreeMap::new(),
            }],
        );

        assert!(document.validate().is_err());
    }
}
