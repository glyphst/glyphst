//! Interactive viewer for GPU vector page rendering.
//!
//! Scroll to zoom around the pointer (animated), drag to pan, F to fit,
//! Escape to quit. The window title shows the last frame's CPU+GPU time.
//!
//! cargo run --release -p glyphst-pdf-gpu --example zoom_view -- <pdf> [--page N]

mod common;

use std::{path::PathBuf, sync::Arc, time::Instant};

use anyhow::{Context as _, Result, bail};
use common::LoadedPage;
use glyphst_pdf_gpu::{Camera, GpuPage, OffscreenTarget, Renderer};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Window, WindowId},
};

/// Zoom factor per wheel notch.
const WHEEL_ZOOM: f32 = 1.25;
/// Exponential approach rate of the animated zoom, per second.
const ZOOM_RESPONSE: f32 = 18.0;
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 512.0;

struct View {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    page: GpuPage,
    page_bounds: [f32; 4],
    /// Multisampled attachments; pages resolve straight into the surface.
    attachments: OffscreenTarget,
    origin: [f32; 2],
    scale: f32,
    target_scale: f32,
    /// Window pixel kept fixed while the zoom animates.
    anchor: [f32; 2],
    pointer: [f32; 2],
    drag: Option<[f32; 2]>,
    last_frame: Instant,
}

impl View {
    fn new(event_loop: &ActiveEventLoop, loaded: &LoadedPage) -> Result<Self> {
        let window = Arc::new(
            event_loop.create_window(Window::default_attributes().with_title("zoom_view"))?,
        );
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(event_loop.owned_display_handle()),
        ));
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("zoom_view"),
                required_limits: adapter.limits(),
                ..Default::default()
            }))?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .context("surface is not supported by the adapter")?;
        // Blend in gamma space, as MuPDF does.
        config.format = config.format.remove_srgb_suffix();
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);

        let renderer = Renderer::new(&device, config.format);
        let fit = loaded.fit_scale([config.width, config.height], 0);
        let uploaded = common::upload(&device, &queue, &renderer, loaded, fit * 4.0)?;
        let attachments = surface_attachments(&device, &config);
        let bounds = loaded.prepared.bounds;
        let mut view = Self {
            window,
            surface,
            device,
            queue,
            config,
            renderer,
            page: uploaded.page,
            page_bounds: bounds,
            attachments,
            origin: [0.0; 2],
            scale: fit,
            target_scale: fit,
            anchor: [0.0; 2],
            pointer: [0.0; 2],
            drag: None,
            last_frame: Instant::now(),
        };
        view.fit();
        Ok(view)
    }

    fn fit(&mut self) {
        let [x0, y0, x1, y1] = self.page_bounds;
        let [width, height] = [self.config.width as f32, self.config.height as f32];
        self.scale = (width / (x1 - x0)).min(height / (y1 - y0));
        self.target_scale = self.scale;
        let camera = Camera::centered(
            [(x0 + x1) / 2.0, (y0 + y1) / 2.0],
            self.scale,
            [self.config.width, self.config.height],
        );
        self.origin = camera.origin;
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
        self.attachments = surface_attachments(&self.device, &self.config);
    }

    /// Moves the zoom toward its target, keeping `anchor` fixed on screen.
    fn animate(&mut self, seconds: f32) -> bool {
        if (self.target_scale / self.scale - 1.0).abs() < 1e-4 {
            self.set_scale(self.target_scale);
            return false;
        }
        let step = 1.0 - (-ZOOM_RESPONSE * seconds.min(1.0 / 30.0)).exp();
        let scale = (self.scale.ln() + (self.target_scale.ln() - self.scale.ln()) * step).exp();
        self.set_scale(scale);
        true
    }

    fn set_scale(&mut self, scale: f32) {
        let [ax, ay] = self.anchor;
        let page_x = self.origin[0] + ax / self.scale;
        let page_y = self.origin[1] + ay / self.scale;
        self.scale = scale;
        self.origin = [page_x - ax / scale, page_y - ay / scale];
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        let animating = self.animate((now - self.last_frame).as_secs_f32());
        self.last_frame = now;

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return;
            }
            _ => return,
        };
        let target = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.config.format),
            ..Default::default()
        });
        let camera = Camera {
            origin: self.origin,
            scale: self.scale,
            viewport: [self.config.width, self.config.height],
            rotation: 0,
        };
        let started = Instant::now();
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let stats = {
            let background = wgpu::Color {
                r: 0.35,
                g: 0.35,
                b: 0.37,
                a: 1.0,
            };
            let mut pass = self
                .attachments
                .begin_pass_into(&mut encoder, &target, background);
            self.renderer
                .draw(&self.queue, &self.page, &camera, &mut pass)
        };
        self.queue.submit([encoder.finish()]);
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let elapsed = started.elapsed();
        self.queue.present(frame);

        let source = if stats.used_overview {
            "overview".to_string()
        } else {
            format!("level {}", self.page.level_for_scale(self.scale))
        };
        self.window.set_title(&format!(
            "zoom_view - {:.2} ms - {:.2} px/pt - {source} - {} segments",
            elapsed.as_secs_f64() * 1000.0,
            self.scale,
            stats.segments,
        ));
        if animating {
            self.window.request_redraw();
        }
    }
}

fn surface_attachments(
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
) -> OffscreenTarget {
    OffscreenTarget::new(
        device,
        config.format,
        [config.width, config.height],
        wgpu::TextureUsages::empty(),
    )
}

struct App {
    loaded: LoadedPage,
    view: Option<View>,
    error: Option<anyhow::Error>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.view.is_some() {
            return;
        }
        match View::new(event_loop, &self.loaded) {
            Ok(view) => self.view = Some(view),
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(view) = &mut self.view else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                view.resize(size.width, size.height);
                view.window.request_redraw();
            }
            WindowEvent::RedrawRequested => view.redraw(),
            WindowEvent::CursorMoved { position, .. } => {
                let pointer = [position.x as f32, position.y as f32];
                if let Some(previous) = view.drag {
                    view.origin[0] -= (pointer[0] - previous[0]) / view.scale;
                    view.origin[1] -= (pointer[1] - previous[1]) / view.scale;
                    view.drag = Some(pointer);
                    view.window.request_redraw();
                }
                view.pointer = pointer;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                view.drag = (state == ElementState::Pressed).then_some(view.pointer);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(position) => position.y as f32 / 60.0,
                };
                let [x0, _, x1, _] = view.page_bounds;
                let fit = view.config.width as f32 / (x1 - x0);
                view.anchor = view.pointer;
                view.target_scale = (view.target_scale * WHEEL_ZOOM.powf(notches))
                    .clamp(fit * MIN_ZOOM, fit * MAX_ZOOM);
                view.last_frame = Instant::now();
                view.window.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.logical_key {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Character(ref text) if text.eq_ignore_ascii_case("f") => {
                        view.fit();
                        view.window.request_redraw();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut pdf = PathBuf::new();
    let mut page = 0;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--page" => page = args.next().context("missing page")?.parse::<i32>()? - 1,
            _ => pdf = arg.into(),
        }
    }
    if pdf.as_os_str().is_empty() {
        bail!("usage: zoom_view <pdf> [--page N]");
    }
    let loaded = LoadedPage::open(&pdf, page)?;
    let event_loop = EventLoop::new()?;
    let mut app = App {
        loaded,
        view: None,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
