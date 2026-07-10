use std::ops::Range;

use serde::{Deserialize, Serialize};

pub mod paths;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
