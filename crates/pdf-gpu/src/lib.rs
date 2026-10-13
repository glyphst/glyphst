//! GPU vector rendering of PDF pages captured by MuPDF.
//!
//! [`prepare::PreparedPage`] turns a [`glyphst_pdf::vector::VectorPage`] into
//! page-space buffers once; [`renderer::Renderer`] draws them at any zoom by
//! updating a camera uniform, so every frame is rendered from vectors.

pub mod prepare;
pub mod renderer;
mod target;

pub use prepare::PreparedPage;
pub use renderer::{
    Camera, DrawStats, GpuPage, LayerUpload, OverviewJob, PageUpload, Renderer, SAMPLE_COUNT,
    STENCIL_FORMAT, stencil_attachment,
};
pub use target::OffscreenTarget;
