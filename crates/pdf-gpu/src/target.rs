use crate::renderer::{SAMPLE_COUNT, STENCIL_FORMAT, stencil_attachment};

/// Multisampled color and stencil attachments for drawing pages, with a
/// single-sampled texture they resolve into.
pub struct OffscreenTarget {
    size: [u32; 2],
    msaa: wgpu::TextureView,
    stencil: wgpu::TextureView,
    resolve: wgpu::Texture,
    resolve_view: wgpu::TextureView,
}

impl OffscreenTarget {
    /// `resolve_usage` is added to the resolve texture's render-attachment
    /// usage, for example `COPY_SRC` for readback or `TEXTURE_BINDING` for
    /// sampling it.
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: [u32; 2],
        resolve_usage: wgpu::TextureUsages,
    ) -> Self {
        let size = [size[0].max(1), size[1].max(1)];
        let texture = |samples, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pdf_gpu_offscreen"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let msaa = texture(SAMPLE_COUNT, format, wgpu::TextureUsages::RENDER_ATTACHMENT)
            .create_view(&Default::default());
        let stencil = texture(
            SAMPLE_COUNT,
            STENCIL_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
        .create_view(&Default::default());
        let resolve = texture(
            1,
            format,
            wgpu::TextureUsages::RENDER_ATTACHMENT | resolve_usage,
        );
        let resolve_view = resolve.create_view(&Default::default());
        Self {
            size,
            msaa,
            stencil,
            resolve,
            resolve_view,
        }
    }

    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    /// The resolved image.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.resolve
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.resolve_view
    }

    /// Begins a page pass cleared to `clear` that resolves into this
    /// target's texture.
    pub fn begin_pass<'a>(
        &'a self,
        encoder: &'a mut wgpu::CommandEncoder,
        clear: wgpu::Color,
    ) -> wgpu::RenderPass<'a> {
        self.begin_pass_into(encoder, &self.resolve_view, clear)
    }

    /// Begins a page pass that resolves into `resolve`, which must have this
    /// target's size and format (for example a window surface).
    pub fn begin_pass_into<'a>(
        &'a self,
        encoder: &'a mut wgpu::CommandEncoder,
        resolve: &'a wgpu::TextureView,
        clear: wgpu::Color,
    ) -> wgpu::RenderPass<'a> {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pdf_gpu_page"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.msaa,
                resolve_target: Some(resolve),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Discard,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: Some(stencil_attachment(&self.stencil)),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    }
}
