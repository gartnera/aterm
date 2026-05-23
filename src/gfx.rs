use std::sync::Arc;

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use winit::window::Window;

use crate::quad::{Quad, QuadPipeline};
use crate::terminal::{GridSnapshot, TerminalSession};

const PAD_X: f32 = 6.0;
const PAD_Y: f32 = 4.0;

struct RowSpan<'a> {
    range: std::ops::Range<usize>,
    attrs: Attrs<'a>,
}

fn build_row_text<'a>(
    row: &[crate::terminal::SnapCell],
    row_idx: usize,
    snap: &GridSnapshot,
    family: Family<'a>,
) -> (String, Vec<RowSpan<'a>>) {
    let mut text = String::with_capacity(row.len() * 2);
    let mut spans: Vec<RowSpan<'_>> = Vec::new();
    let cursor_here = snap.cursor_visible && snap.cursor_line == row_idx;

    for (col, cell) in row.iter().enumerate() {
        // Skip wide-char-spacer slots; the wide glyph in the previous column
        // already advances two columns visually.
        let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
        // At the cursor cell we want the glyph to read against the cursor
        // block, so invert fg to the cell's bg (typically the terminal bg).
        let on_cursor = cursor_here && col == snap.cursor_col;
        let fg = if on_cursor { cell.bg } else { cell.fg };
        let start = text.len();
        // Push the actual char; pad with a NBSP if the cell is empty (cosmic-text
        // collapses runs of spaces in some shaping paths, which can drift the
        // column grid).
        text.push(if ch == ' ' { '\u{00A0}' } else { ch });
        let end = text.len();
        let mut attrs = Attrs::new().family(family);
        attrs = attrs.color(Color::rgb(fg[0], fg[1], fg[2]));
        if cell.bold {
            attrs = attrs.weight(glyphon::Weight::BOLD);
        }
        if cell.italic {
            attrs = attrs.style(glyphon::Style::Italic);
        }
        spans.push(RowSpan { range: start..end, attrs });
    }
    (text, spans)
}

fn measure_cell_width(
    font_system: &mut FontSystem,
    font_size: f32,
    line_height: f32,
    family_name: &str,
) -> f32 {
    let metrics = Metrics::new(font_size, line_height);
    let mut buf = Buffer::new(font_system, metrics);
    buf.set_size(font_system, Some(1000.0), Some(line_height + 4.0));
    let family = if family_name.eq_ignore_ascii_case("monospace") {
        Family::Monospace
    } else {
        Family::Name(family_name)
    };
    buf.set_text(
        font_system,
        "MMMMMMMMMM",
        &Attrs::new().family(family),
        Shaping::Advanced,
        None,
    );
    buf.shape_until_scroll(font_system, false);
    let max_x = buf
        .layout_runs()
        .flat_map(|run| run.glyphs.iter())
        .map(|g| g.x + g.w)
        .fold(0.0_f32, f32::max);
    if max_x > 0.0 {
        max_x / 10.0
    } else {
        font_size * 0.6
    }
}

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
    cell_width_logical: f32,
    font_family: String,
    /// Physical-pixel x-ranges for each rendered tab, refreshed every frame.
    tab_hit_regions: Vec<(usize, f32, f32)>,
    quads: QuadPipeline,
}

impl Gfx {
    pub async fn new(
        window: Arc<Window>,
        font_size: f32,
        line_height: f32,
        font_family: String,
    ) -> Self {
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
        let adapter_limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("alacritty-tabs device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter_limits,
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

        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cell_width_logical =
            measure_cell_width(&mut font_system, font_size, line_height, &font_family);
        let quads = QuadPipeline::new(&device, format);
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
            cell_width_logical,
            font_family,
            tab_hit_regions: Vec::new(),
            quads,
        }
    }

    /// Returns the tab index at the given physical-pixel x within the tab bar.
    pub fn tab_at_x(&self, x_px: f32) -> Option<usize> {
        self.tab_hit_regions
            .iter()
            .find(|(_, x0, x1)| x_px >= *x0 && x_px <= *x1)
            .map(|(idx, _, _)| *idx)
    }


    /// Cell dimensions in *logical* pixels (pre-scale-factor).
    pub fn cell_dims_logical(&self) -> (f32, f32) {
        (self.cell_width_logical, self.line_height)
    }

    /// Compute the grid (cols, lines) that fit in the current window below the
    /// tab bar.
    pub fn grid_for_window(&self, tab_bar_height: f32) -> (u16, u16) {
        let scale = self.window.scale_factor() as f32;
        let w_px = self.surface_config.width as f32;
        let h_px = self.surface_config.height as f32;
        let usable_w = (w_px - 2.0 * PAD_X * scale).max(0.0);
        let usable_h = (h_px - (tab_bar_height + 2.0 * PAD_Y) * scale).max(0.0);
        let cw = (self.cell_width_logical * scale).max(1.0);
        let ch = (self.line_height * scale).max(1.0);
        let cols = (usable_w / cw).floor().max(2.0) as u16;
        let lines = (usable_h / ch).floor().max(2.0) as u16;
        (cols, lines)
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
        active: Option<&TerminalSession>,
        tabs: &[TerminalSession],
        active_idx: usize,
        tab_bar_height: f32,
    ) -> Result<(), String> {
        let width = self.surface_config.width;
        let height = self.surface_config.height;
        self.viewport.update(&self.queue, Resolution { width, height });

        let scale = self.window.scale_factor() as f32;
        let cell_w_px = self.cell_width_logical * scale;
        let cell_h_px = self.line_height * scale;
        let metrics = Metrics::new(self.font_size * scale, self.line_height * scale);

        // Snapshot the active terminal once outside the prepare call.
        let snapshot = active.map(|t| t.snapshot());
        let default_fg = snapshot
            .as_ref()
            .map(|s| Color::rgb(s.fg[0], s.fg[1], s.fg[2]))
            .unwrap_or_else(|| Color::rgb(0xd0, 0xd0, 0xd0));

        let mut quads: Vec<Quad> = Vec::new();
        // Tab bar background strip.
        quads.push(Quad {
            rect: [0.0, 0.0, width as f32, tab_bar_height * scale],
            color: [0.16, 0.16, 0.20, 1.0],
        });

        // Collect (Buffer, x_px, y_px, default_color) entries; build them first
        // so all borrows of font_system are released before we hand the Vec to
        // glyphon's prepare() (which also borrows font_system mutably).
        #[allow(clippy::type_complexity)]
        let mut entries: Vec<(Buffer, f32, f32, Color)> = Vec::new();

        // Borrow font_family as a local &str so we can build Family<'_> values
        // without re-borrowing &self alongside &mut self.font_system.
        let family_name: String = self.font_family.clone();
        fn family_of(name: &str) -> Family<'_> {
            if name.eq_ignore_ascii_case("monospace") {
                Family::Monospace
            } else {
                Family::Name(name)
            }
        }

        // Tab bar: build the text and capture per-tab character ranges so we
        // can hit-test mouse clicks back to a tab index.
        self.tab_hit_regions.clear();
        let mut tab_text = String::new();
        for (i, t) in tabs.iter().enumerate() {
            if i > 0 {
                tab_text.push_str("   ");
            }
            let chars_before = tab_text.chars().count();
            let marker = if i == active_idx { "● " } else { "○ " };
            tab_text.push_str(&format!("{marker}{}", t.title()));
            let chars_after = tab_text.chars().count();
            let x0 = PAD_X * scale + chars_before as f32 * cell_w_px;
            let x1 = PAD_X * scale + chars_after as f32 * cell_w_px;
            self.tab_hit_regions.push((i, x0, x1));
            if i == active_idx {
                quads.push(Quad {
                    rect: [x0 - 4.0, 0.0, (x1 - x0) + 8.0, tab_bar_height * scale],
                    color: [0.24, 0.24, 0.30, 1.0],
                });
            }
        }
        if tab_text.is_empty() {
            tab_text.push_str("(no tabs)");
        }
        let mut tab_buf = Buffer::new(&mut self.font_system, metrics);
        tab_buf.set_size(
            &mut self.font_system,
            Some(width as f32),
            Some(tab_bar_height * scale),
        );
        tab_buf.set_text(
            &mut self.font_system,
            &tab_text,
            &Attrs::new().family(family_of(&family_name)),
            Shaping::Advanced,
            None,
        );
        tab_buf.shape_until_scroll(&mut self.font_system, false);
        entries.push((
            tab_buf,
            PAD_X * scale,
            (tab_bar_height - self.line_height) * 0.5 * scale,
            Color::rgb(0xc8, 0xc8, 0xd0),
        ));

        // Grid rows: one Buffer per visible line.
        let top_offset_px = (tab_bar_height + PAD_Y) * scale;
        if let Some(snap) = snapshot.as_ref() {
            if snap.cursor_visible {
                let x = PAD_X * scale + snap.cursor_col as f32 * cell_w_px;
                let y = top_offset_px + snap.cursor_line as f32 * cell_h_px;
                let fg = snap.fg;
                quads.push(Quad {
                    rect: [x, y, cell_w_px, cell_h_px],
                    color: [
                        fg[0] as f32 / 255.0,
                        fg[1] as f32 / 255.0,
                        fg[2] as f32 / 255.0,
                        1.0,
                    ],
                });
            }
            for (row_idx, row) in snap.cells.iter().enumerate() {
                let mut buf = Buffer::new(&mut self.font_system, metrics);
                buf.set_size(
                    &mut self.font_system,
                    Some(cell_w_px * row.len() as f32 + cell_w_px),
                    Some(cell_h_px + 2.0),
                );
                let (text, spans_meta) =
                    build_row_text(row, row_idx, snap, family_of(&family_name));
                let spans = spans_meta.iter().map(|s| (&text[s.range.clone()], s.attrs.clone()));
                buf.set_rich_text(
                    &mut self.font_system,
                    spans,
                    &Attrs::new().family(family_of(&family_name)),
                    Shaping::Advanced,
                    None,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
                let y = top_offset_px + row_idx as f32 * cell_h_px;
                entries.push((buf, PAD_X * scale, y, default_fg));
            }
        }

        let text_areas: Vec<TextArea<'_>> = entries
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

        self.quads.upload(&self.device, &self.queue, width, height, &quads);

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.surface_config);
                return Ok(());
            }
            // Occluded (window hidden) and Timeout aren't errors — skip the frame.
            wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Timeout => {
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
            self.quads.render(&mut pass);
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
