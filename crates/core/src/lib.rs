use std::ops::Range;

use serde::{Deserialize, Serialize};

pub mod paths;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DocumentFormat {
    Pdf,
    Epub,
}

impl DocumentFormat {
    pub fn from_extension(extension: &str) -> Option<Self> {
        if extension.eq_ignore_ascii_case("pdf") {
            Some(Self::Pdf)
        } else if extension.eq_ignore_ascii_case("epub") {
            Some(Self::Epub)
        } else {
            None
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Epub => "application/epub+zip",
        }
    }

    pub fn is_reflowable(self) -> bool {
        matches!(self, Self::Epub)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DocumentRotation {
    #[default]
    Degrees0,
    Degrees90,
    Degrees180,
    Degrees270,
}

impl DocumentRotation {
    pub const fn clockwise(self) -> Self {
        match self {
            Self::Degrees0 => Self::Degrees90,
            Self::Degrees90 => Self::Degrees180,
            Self::Degrees180 => Self::Degrees270,
            Self::Degrees270 => Self::Degrees0,
        }
    }

    pub const fn counterclockwise(self) -> Self {
        match self {
            Self::Degrees0 => Self::Degrees270,
            Self::Degrees90 => Self::Degrees0,
            Self::Degrees180 => Self::Degrees90,
            Self::Degrees270 => Self::Degrees180,
        }
    }

    pub const fn quarter_turns(self) -> u8 {
        match self {
            Self::Degrees0 => 0,
            Self::Degrees90 => 1,
            Self::Degrees180 => 2,
            Self::Degrees270 => 3,
        }
    }

    pub const fn swaps_width_and_height(self) -> bool {
        matches!(self, Self::Degrees90 | Self::Degrees270)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentCapabilities {
    pub reflow: bool,
    pub publisher_styles: bool,
    pub page_overview: bool,
    pub page_export: bool,
    pub presentation: bool,
    pub reload_diff: bool,
    pub translated_document: bool,
    pub translation_split: bool,
}

impl DocumentCapabilities {
    pub fn for_document(format: DocumentFormat, reflowable: bool) -> Self {
        let fixed_layout = !reflowable;
        Self {
            reflow: reflowable,
            publisher_styles: format == DocumentFormat::Epub && reflowable,
            page_overview: fixed_layout || (format == DocumentFormat::Epub && reflowable),
            page_export: fixed_layout,
            presentation: fixed_layout,
            reload_diff: fixed_layout,
            translated_document: fixed_layout,
            translation_split: fixed_layout,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EpubTheme {
    #[default]
    Light,
    Dark,
    Sepia,
}

impl EpubTheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Sepia => "sepia",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "sepia" => Some(Self::Sepia),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EpubLayout {
    pub width: f32,
    pub height: f32,
    pub font_size: f32,
    pub line_height: f32,
    pub horizontal_margin: f32,
    pub theme: EpubTheme,
    pub use_publisher_styles: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EpubReaderPreferences {
    pub font_size: f32,
    pub line_height: f32,
    pub horizontal_margin: f32,
    pub max_content_width: f32,
    pub theme: EpubTheme,
    pub use_publisher_styles: bool,
}

impl Default for EpubReaderPreferences {
    fn default() -> Self {
        let layout = EpubLayout::default();
        Self {
            font_size: layout.font_size,
            line_height: layout.line_height,
            horizontal_margin: layout.horizontal_margin,
            max_content_width: layout.width,
            theme: layout.theme,
            use_publisher_styles: layout.use_publisher_styles,
        }
    }
}

impl EpubReaderPreferences {
    pub fn sanitized(mut self) -> Self {
        let defaults = Self::default();
        self.font_size = finite_or(self.font_size, defaults.font_size).clamp(8.0, 48.0);
        self.line_height = finite_or(self.line_height, defaults.line_height).clamp(1.0, 3.0);
        self.horizontal_margin =
            finite_or(self.horizontal_margin, defaults.horizontal_margin).clamp(0.0, 256.0);
        self.max_content_width =
            finite_or(self.max_content_width, defaults.max_content_width).clamp(240.0, 2048.0);
        self
    }

    pub fn layout_for_viewport(self, viewport_width: f32, viewport_height: f32) -> EpubLayout {
        let preferences = self.sanitized();
        EpubLayout {
            width: finite_or(viewport_width, preferences.max_content_width)
                .min(preferences.max_content_width)
                .max(240.0),
            height: finite_or(viewport_height, EpubLayout::default().height).clamp(320.0, 4096.0),
            font_size: preferences.font_size,
            line_height: preferences.line_height,
            horizontal_margin: preferences.horizontal_margin,
            theme: preferences.theme,
            use_publisher_styles: preferences.use_publisher_styles,
        }
        .sanitized()
    }
}

impl Default for EpubLayout {
    fn default() -> Self {
        Self {
            width: 720.0,
            height: 960.0,
            font_size: 16.0,
            line_height: 1.5,
            horizontal_margin: 32.0,
            theme: EpubTheme::Light,
            use_publisher_styles: true,
        }
    }
}

impl EpubLayout {
    pub fn sanitized(mut self) -> Self {
        let defaults = Self::default();
        self.width = finite_or(self.width, defaults.width).clamp(240.0, 4096.0);
        self.height = finite_or(self.height, defaults.height).clamp(320.0, 4096.0);
        self.font_size = finite_or(self.font_size, defaults.font_size).clamp(8.0, 48.0);
        self.line_height = finite_or(self.line_height, defaults.line_height).clamp(1.0, 3.0);
        self.horizontal_margin =
            finite_or(self.horizontal_margin, defaults.horizontal_margin).clamp(0.0, 256.0);
        self
    }

    pub fn user_css(self) -> String {
        let layout = self.sanitized();
        let (background, foreground, link) = match layout.theme {
            EpubTheme::Light => ("#ffffff", "#1f2328", "#0969da"),
            EpubTheme::Dark => ("#1e1e2e", "#cdd6f4", "#89b4fa"),
            EpubTheme::Sepia => ("#f4ecd8", "#493b2a", "#8b5e34"),
        };
        format!(
            "html, body {{ background: {background}; color: {foreground}; }} \
             body {{ margin-left: {margin}px; margin-right: {margin}px; \
             line-height: {line_height}; }} \
             a {{ color: {link}; }} img, svg {{ max-width: 100%; height: auto; }}",
            margin = layout.horizontal_margin,
            line_height = layout.line_height,
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum DocumentLayout {
    #[default]
    Fixed,
    Reflow(EpubLayout),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticTextAnchor {
    pub logical_offset: u64,
    pub quote: String,
    pub prefix: String,
    pub suffix: String,
}

impl SemanticTextAnchor {
    pub fn empty() -> Self {
        Self::new(0, "", "", "")
    }

    pub fn new(
        logical_offset: usize,
        quote: impl Into<String>,
        prefix: impl Into<String>,
        suffix: impl Into<String>,
    ) -> Self {
        Self {
            logical_offset: logical_offset as u64,
            quote: quote.into(),
            prefix: prefix.into(),
            suffix: suffix.into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.logical_offset == 0
            && self.quote.is_empty()
            && self.prefix.is_empty()
            && self.suffix.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocumentId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PageIndex(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageRegion {
    pub page: PageIndex,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl PageRegion {
    pub fn normalized(self) -> Self {
        Self {
            page: self.page,
            x0: self.x0.min(self.x1),
            y0: self.y0.min(self.y1),
            x1: self.x0.max(self.x1),
            y1: self.y0.max(self.y1),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegionImageReference {
    pub regions: Vec<PageRegion>,
}

impl RegionImageReference {
    pub fn new(regions: impl IntoIterator<Item = PageRegion>) -> Self {
        let mut regions = regions
            .into_iter()
            .map(PageRegion::normalized)
            .collect::<Vec<_>>();
        regions.sort_by(|left, right| {
            left.page
                .0
                .cmp(&right.page.0)
                .then_with(|| left.y0.total_cmp(&right.y0))
                .then_with(|| left.x0.total_cmp(&right.x0))
        });
        Self { regions }
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentLocation {
    pub page: PageIndex,
    pub y_in_page: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewportState {
    pub location: DocumentLocation,
    pub scroll_x: f32,
    pub zoom: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageArtifact {
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextSpan {
    pub text: String,
    pub regions: Vec<PageRegion>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextSelection {
    pub text: String,
    pub range: Range<usize>,
    pub regions: Vec<PageRegion>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HighlightColor {
    Yellow,
    Green,
    Blue,
    Pink,
    Orange,
}

impl HighlightColor {
    pub const ALL: [Self; 5] = [
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Pink,
        Self::Orange,
    ];
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_document_extensions() {
        assert_eq!(
            DocumentFormat::from_extension("PDF"),
            Some(DocumentFormat::Pdf)
        );
        assert_eq!(
            DocumentFormat::from_extension("ePuB"),
            Some(DocumentFormat::Epub)
        );
        assert_eq!(DocumentFormat::from_extension("txt"), None);
    }

    #[test]
    fn document_rotation_cycles_in_both_directions() {
        assert_eq!(DocumentRotation::default().quarter_turns(), 0);
        assert_eq!(
            DocumentRotation::Degrees0.clockwise(),
            DocumentRotation::Degrees90
        );
        assert_eq!(
            DocumentRotation::Degrees90.clockwise(),
            DocumentRotation::Degrees180
        );
        assert_eq!(
            DocumentRotation::Degrees180.clockwise(),
            DocumentRotation::Degrees270
        );
        assert_eq!(
            DocumentRotation::Degrees270.clockwise(),
            DocumentRotation::Degrees0
        );
        assert_eq!(
            DocumentRotation::Degrees0.counterclockwise(),
            DocumentRotation::Degrees270
        );
        assert_eq!(
            DocumentRotation::Degrees270.counterclockwise(),
            DocumentRotation::Degrees180
        );
        assert_eq!(
            DocumentRotation::Degrees180.counterclockwise(),
            DocumentRotation::Degrees90
        );
        assert_eq!(
            DocumentRotation::Degrees90.counterclockwise(),
            DocumentRotation::Degrees0
        );
    }

    #[test]
    fn quarter_turn_rotation_reports_dimension_swaps() {
        assert!(!DocumentRotation::Degrees0.swaps_width_and_height());
        assert!(DocumentRotation::Degrees90.swaps_width_and_height());
        assert!(!DocumentRotation::Degrees180.swaps_width_and_height());
        assert!(DocumentRotation::Degrees270.swaps_width_and_height());
    }

    #[test]
    fn identifies_empty_semantic_anchor_sentinel() {
        assert!(SemanticTextAnchor::empty().is_empty());
        assert!(!SemanticTextAnchor::new(4, "text", "", "").is_empty());
    }

    #[test]
    fn capabilities_follow_layout_model() {
        let epub = DocumentCapabilities::for_document(DocumentFormat::Epub, true);
        assert!(epub.reflow);
        assert!(epub.publisher_styles);
        assert!(epub.page_overview);
        assert!(!epub.page_export);
        assert!(!epub.presentation);

        let fixed_epub = DocumentCapabilities::for_document(DocumentFormat::Epub, false);
        assert!(!fixed_epub.reflow);
        assert!(fixed_epub.page_overview);
        assert!(fixed_epub.page_export);
        assert!(!fixed_epub.publisher_styles);
    }

    #[test]
    fn sanitizes_epub_layout() {
        let layout = EpubLayout {
            width: f32::NAN,
            height: 10.0,
            font_size: 100.0,
            line_height: 0.5,
            horizontal_margin: f32::INFINITY,
            ..EpubLayout::default()
        }
        .sanitized();

        assert_eq!(layout.width, EpubLayout::default().width);
        assert_eq!(layout.height, 320.0);
        assert_eq!(layout.font_size, 48.0);
        assert_eq!(layout.line_height, 1.0);
        assert_eq!(
            layout.horizontal_margin,
            EpubLayout::default().horizontal_margin
        );
    }

    #[test]
    fn reader_preferences_constrain_layout_to_readable_width() {
        let layout = EpubReaderPreferences::default().layout_for_viewport(1400.0, 900.0);
        assert_eq!(
            layout.width,
            EpubReaderPreferences::default().max_content_width
        );
        assert_eq!(layout.height, 900.0);
    }

    #[test]
    fn normalizes_page_regions() {
        assert_eq!(
            PageRegion {
                page: PageIndex(3),
                x0: 8.0,
                y0: 4.0,
                x1: 2.0,
                y1: 1.0,
            }
            .normalized(),
            PageRegion {
                page: PageIndex(3),
                x0: 2.0,
                y0: 1.0,
                x1: 8.0,
                y1: 4.0,
            }
        );
    }

    #[test]
    fn region_image_references_normalize_and_sort_pages() {
        let reference = RegionImageReference::new([
            PageRegion {
                page: PageIndex(2),
                x0: 8.0,
                y0: 7.0,
                x1: 2.0,
                y1: 1.0,
            },
            PageRegion {
                page: PageIndex(0),
                x0: 4.0,
                y0: 3.0,
                x1: 1.0,
                y1: 2.0,
            },
        ]);

        assert_eq!(reference.regions[0].page, PageIndex(0));
        assert_eq!(reference.regions[0].x0, 1.0);
        assert_eq!(reference.regions[1].page, PageIndex(2));
        assert_eq!(reference.regions[1].y0, 1.0);
    }
}
