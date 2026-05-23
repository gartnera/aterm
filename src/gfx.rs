use std::sync::Arc;

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use winit::window::Window;

use crate::terminal::TerminalSession;

pub struct Gfx {
    window: Arc<Window>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    font_system: FontSystem,
    swash_cache: SwashCache,
    text_atlas: TextAtlas,
    text_renderer: TextRenderer,
    viewport: Viewport,
    font_size: f32,
    line_height: f32,
}

impl Gfx {
    pub async fn new(window: Arc<Window>, font_size: f32, line_height: f32) -> Self {
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone()).expect("create surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("no adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("alacritty-tabs device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            })
            .await
            .expect("request_device");
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        let font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut text_atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer = TextRenderer::new(
            &mut text_atlas,
            &device,
            wgpu::MultisampleState::default(),
            None,
        );

        Self {
            window,
            device,
            queue,
            surface,
            surface_config,
            font_system,
            swash_cache,
            text_atlas,
            text_renderer,
            viewport,
            font_size,
            line_height,
        }
    }

    pub fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.surface_config.width = size.width;
        self.surface_config.height = size.height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    pub fn render(
        &mut self,
        _active: Option<&TerminalSession>,
        tabs: &[TerminalSession],
        active_idx: usize,
        tab_bar_height: f32,
    ) -> Result<(), String> {
        let width = self.surface_config.width;
        let height = self.surface_config.height;
        self.viewport.update(&self.queue, Resolution { width, height });

        let scale = self.window.scale_factor() as f32;
        let mut buffers: Vec<(Buffer, f32, f32, Color)> = Vec::new();

        // Tab bar text.
        let mut tab_text = String::new();
        for (i, t) in tabs.iter().enumerate() {
            if i > 0 {
                tab_text.push_str(" | ");
            }
            let marker = if i == active_idx { "*" } else { " " };
            tab_text.push_str(&format!("{marker}{}", t.title()));
        }
        if tab_text.is_empty() {
            tab_text = "(no tabs)".into();
        }
        let metrics = Metrics::new(self.font_size * scale, self.line_height * scale);
        let mut tab_buf = Buffer::new(&mut self.font_system, metrics);
        tab_buf.set_size(
            &mut self.font_system,
            Some(width as f32),
            Some(tab_bar_height * scale),
        );
        tab_buf.set_text(
            &mut self.font_system,
            &tab_text,
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        tab_buf.shape_until_scroll(&mut self.font_system, false);
        buffers.push((tab_buf, 8.0 * scale, 4.0 * scale, Color::rgb(0xe0, 0xe0, 0xe0)));

        // Placeholder body text — actual grid rendering will replace this.
        let body_metrics = Metrics::new(self.font_size * scale, self.line_height * scale);
        let mut body_buf = Buffer::new(&mut self.font_system, body_metrics);
        body_buf.set_size(
            &mut self.font_system,
            Some((width as f32) - 16.0 * scale),
            Some((height as f32) - (tab_bar_height + 8.0) * scale),
        );
        let placeholder =
            "terminal grid rendering not wired up yet — type to verify input forwarding works\n";
        body_buf.set_text(
            &mut self.font_system,
            placeholder,
            &Attrs::new().family(Family::Monospace),
            Shaping::Advanced,
            None,
        );
        body_buf.shape_until_scroll(&mut self.font_system, false);
        buffers.push((
            body_buf,
            8.0 * scale,
            (tab_bar_height + 4.0) * scale,
            Color::rgb(0xd0, 0xd0, 0xd0),
        ));

        let text_areas: Vec<TextArea<'_>> = buffers
            .iter()
            .map(|(buf, x, y, color)| TextArea {
                buffer: buf,
                left: *x,
                top: *y,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: width as i32,
                    bottom: height as i32,
                },
                default_color: *color,
                custom_glyphs: &[],
            })
            .collect();

        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.text_atlas,
                &self.viewport,
                text_areas,
                &mut self.swash_cache,
            )
            .expect("prepare text");

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.surface_config);
                return Ok(());
            }
            other => return Err(format!("surface texture unavailable: {other:?}")),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.06,
                            g: 0.06,
                            b: 0.08,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.text_renderer
                .render(&self.text_atlas, &self.viewport, &mut pass)
                .expect("render text");
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.text_atlas.trim();
        Ok(())
    }
}
