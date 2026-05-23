use std::sync::Arc;

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use winit::window::Window;

use crate::config::Colors as ConfigColors;
use crate::quad::{Quad, QuadPipeline};
use crate::terminal::{GridSnapshot, TerminalSession};

struct TabBarTheme {
    /// Background of the strip behind all tabs (linear-space wgpu color).
    bar_bg: [f32; 4],
    /// Background of the active tab — matches the terminal content bg so the
    /// active tab visually merges into the page.
    active_bg: [f32; 4],
    /// Text color for the active tab (sRGB; glyphon uses sRGB).
    active_fg: [u8; 3],
    /// Text color for inactive tabs and separators.
    inactive_fg: [u8; 3],
}

impl TabBarTheme {
    fn derive(colors: &ConfigColors) -> Self {
        Self {
            bar_bg: linear_rgba(darken(colors.background, 0.7)),
            active_bg: linear_rgba(colors.background),
            active_fg: colors.foreground,
            inactive_fg: colors.bright.black,
        }
    }
}

fn darken([r, g, b]: [u8; 3], factor: f32) -> [u8; 3] {
    let f = factor.clamp(0.0, 1.0);
    [
        (r as f32 * f).round() as u8,
        (g as f32 * f).round() as u8,
        (b as f32 * f).round() as u8,
    ]
}

fn linear_rgba(c: [u8; 3]) -> [f32; 4] {
    [
        srgb_to_linear(c[0]) as f32,
        srgb_to_linear(c[1]) as f32,
        srgb_to_linear(c[2]) as f32,
        1.0,
    ]
}

const PAD_X: f32 = 6.0;
const PAD_Y: f32 = 4.0;

struct RowSpan<'a> {
    range: std::ops::Range<usize>,
    attrs: Attrs<'a>,
}

fn family_of(name: &str) -> Family<'_> {
    if name.eq_ignore_ascii_case("monospace") {
        Family::Monospace
    } else {
        Family::Name(name)
    }
}

fn srgb_to_linear(c: u8) -> f64 {
    let v = c as f64 / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if max_chars == 0 {
        return String::new();
    }
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars == 1 {
        return "…".into();
    }
    let head: String = s.chars().take(max_chars - 1).collect();
    format!("{head}…")
}

fn push_bg_quad(
    quads: &mut Vec<Quad>,
    scale: f32,
    cell_w_px: f32,
    cell_h_px: f32,
    start_col: usize,
    end_col: usize,
    y_px: f32,
    bg: [u8; 3],
) {
    let x = PAD_X * scale + start_col as f32 * cell_w_px;
    let w = (end_col - start_col) as f32 * cell_w_px;
    quads.push(Quad {
        rect: [x, y_px, w, cell_h_px],
        color: [
            bg[0] as f32 / 255.0,
            bg[1] as f32 / 255.0,
            bg[2] as f32 / 255.0,
            1.0,
        ],
    });
}

fn cell_in_selection(snap: &GridSnapshot, row: usize, col: usize) -> bool {
    let Some(sel) = snap.selection else { return false };
    if sel.is_block {
        return row >= sel.start_line
            && row <= sel.end_line
            && col >= sel.start_col
            && col <= sel.end_col;
    }
    if row < sel.start_line || row > sel.end_line {
        return false;
    }
    if sel.start_line == sel.end_line {
        col >= sel.start_col && col <= sel.end_col
    } else if row == sel.start_line {
        col >= sel.start_col
    } else if row == sel.end_line {
        col <= sel.end_col
    } else {
        true
    }
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
        let selected = cell_in_selection(snap, row_idx, col);
        let fg = if on_cursor {
            cell.bg
        } else if selected {
            // Render text against the selection highlight by swapping fg/bg.
            cell.bg
        } else {
            cell.fg
        };
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
    clear_color: wgpu::Color,
    tab_theme: TabBarTheme,
    /// Physical-pixel x-ranges for each rendered tab, refreshed every frame.
    tab_hit_regions: Vec<(usize, f32, f32)>,
    quads: QuadPipeline,
    /// Reusable per-row text buffers; grown as the grid grows. Recreating
    /// these every frame was the dominant per-frame cost.
    row_buffers: Vec<Buffer>,
    tab_buffer: Option<Buffer>,
    /// Reusable quad accumulator so we don't reallocate Vec<Quad> per frame.
    quad_scratch: Vec<Quad>,
}

impl Gfx {
    pub async fn new(
        window: Arc<Window>,
        font_size: f32,
        line_height: f32,
        font_family: String,
        colors: ConfigColors,
    ) -> Self {
        let bg = colors.background;
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
            clear_color: wgpu::Color {
                r: srgb_to_linear(bg[0]),
                g: srgb_to_linear(bg[1]),
                b: srgb_to_linear(bg[2]),
                a: 1.0,
            },
            tab_theme: TabBarTheme::derive(&colors),
            tab_hit_regions: Vec::new(),
            quads,
            row_buffers: Vec::new(),
            tab_buffer: None,
            quad_scratch: Vec::new(),
        }
    }

    /// Returns the tab index at the given physical-pixel x within the tab bar.
    pub fn tab_at_x(&self, x_px: f32) -> Option<usize> {
        self.tab_hit_regions
            .iter()
            .find(|(_, x0, x1)| x_px >= *x0 && x_px <= *x1)
            .map(|(idx, _, _)| *idx)
    }

    /// Map a window-relative physical pixel position to a (viewport_line,
    /// viewport_col, right_half) cell coordinate inside the terminal grid.
    /// Returns `None` if the position is above the grid (in the tab bar) or
    /// the grid has zero cells.
    pub fn cell_at(
        &self,
        x_px: f32,
        y_px: f32,
        tab_bar_height: f32,
        cols: u16,
        lines: u16,
    ) -> Option<(usize, usize, bool)> {
        let scale = self.window.scale_factor() as f32;
        let cw = self.cell_width_logical * scale;
        let ch = self.line_height * scale;
        if cw <= 0.0 || ch <= 0.0 || cols == 0 || lines == 0 {
            return None;
        }
        let pad_x = PAD_X * scale;
        let top = (tab_bar_height + PAD_Y) * scale;
        if y_px < top {
            return None;
        }
        let x_rel = (x_px - pad_x).max(0.0);
        let y_rel = y_px - top;
        let col_f = x_rel / cw;
        let line = (y_rel / ch).floor() as i64;
        let line = line.clamp(0, lines as i64 - 1) as usize;
        let col_floor = col_f.floor();
        let col = col_floor.clamp(0.0, cols as f32 - 1.0) as usize;
        let frac = col_f - col_floor;
        Some((line, col, frac >= 0.5))
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

        let quads = &mut self.quad_scratch;
        quads.clear();
        quads.push(Quad {
            rect: [0.0, 0.0, width as f32, tab_bar_height * scale],
            color: self.tab_theme.bar_bg,
        });

        let family_name = self.font_family.clone();

        // ===== Tab bar text + hit regions + active-tab quad. =====
        // Distribute available width across tabs and truncate titles that
        // exceed their share. Each tab uses 2 chars for the marker plus the
        // title, separated by 3 spaces between tabs.
        const SEP: &str = "   ";
        const MARKER_CHARS: usize = 2;
        let usable_chars = ((width as f32 - 2.0 * PAD_X * scale) / cell_w_px)
            .floor()
            .max(0.0) as usize;
        let per_tab_budget = if tabs.is_empty() {
            0
        } else {
            let total_seps = tabs.len().saturating_sub(1) * SEP.chars().count();
            usable_chars
                .saturating_sub(total_seps)
                .checked_div(tabs.len())
                .unwrap_or(0)
                .max(MARKER_CHARS + 1)
        };

        self.tab_hit_regions.clear();
        // Build the tab strip as (text segment, is_active) pairs so we can
        // emit each with the correct fg color via set_rich_text.
        let mut segments: Vec<(String, bool)> = Vec::new();
        let mut tab_text = String::new();
        for (i, t) in tabs.iter().enumerate() {
            if i > 0 {
                segments.push((SEP.into(), false));
                tab_text.push_str(SEP);
            }
            let chars_before = tab_text.chars().count();
            let marker = if i == active_idx { "● " } else { "○ " };
            let max_title_chars = per_tab_budget.saturating_sub(MARKER_CHARS);
            let title = truncate_with_ellipsis(t.title(), max_title_chars);
            let seg = format!("{marker}{title}");
            tab_text.push_str(&seg);
            let chars_after = tab_text.chars().count();
            let x0 = PAD_X * scale + chars_before as f32 * cell_w_px;
            let x1 = PAD_X * scale + chars_after as f32 * cell_w_px;
            self.tab_hit_regions.push((i, x0, x1));
            if i == active_idx {
                quads.push(Quad {
                    rect: [x0 - 4.0, 0.0, (x1 - x0) + 8.0, tab_bar_height * scale],
                    color: self.tab_theme.active_bg,
                });
            }
            segments.push((seg, i == active_idx));
        }
        if segments.is_empty() {
            segments.push(("(no tabs)".into(), false));
        }

        // Reuse the cached tab buffer.
        if self.tab_buffer.is_none() {
            self.tab_buffer = Some(Buffer::new(&mut self.font_system, metrics));
        }
        let active_fg = self.tab_theme.active_fg;
        let inactive_fg = self.tab_theme.inactive_fg;
        {
            let buf = self.tab_buffer.as_mut().unwrap();
            buf.set_metrics(&mut self.font_system, metrics);
            buf.set_size(
                &mut self.font_system,
                Some(width as f32),
                Some(tab_bar_height * scale),
            );
            let family = family_of(&family_name);
            let spans = segments.iter().map(|(text, active)| {
                let fg = if *active { active_fg } else { inactive_fg };
                (
                    text.as_str(),
                    Attrs::new()
                        .family(family)
                        .color(Color::rgb(fg[0], fg[1], fg[2])),
                )
            });
            buf.set_rich_text(
                &mut self.font_system,
                spans,
                &Attrs::new().family(family),
                Shaping::Advanced,
                None,
            );
            buf.shape_until_scroll(&mut self.font_system, false);
        }
        let tab_pos = (
            PAD_X * scale,
            (tab_bar_height - self.line_height) * 0.5 * scale,
            Color::rgb(inactive_fg[0], inactive_fg[1], inactive_fg[2]),
        );

        // ===== Grid: background quads, cursor quad, row text buffers. =====
        let top_offset_px = (tab_bar_height + PAD_Y) * scale;
        let mut row_count = 0usize;
        if let Some(snap) = snapshot.as_ref() {
            let default_bg = snap.bg;
            for (row_idx, row) in snap.cells.iter().enumerate() {
                let y = top_offset_px + row_idx as f32 * cell_h_px;
                let cursor_col = (snap.cursor_visible && snap.cursor_line == row_idx)
                    .then_some(snap.cursor_col);
                let mut run: Option<(usize, [u8; 3])> = None;
                for (col, cell) in row.iter().enumerate() {
                    let is_cursor = Some(col) == cursor_col;
                    let bg_opt = if is_cursor || cell.bg == default_bg {
                        None
                    } else {
                        Some(cell.bg)
                    };
                    match (run, bg_opt) {
                        (Some((start, bg)), Some(new_bg)) if bg == new_bg => {
                            run = Some((start, bg));
                        }
                        (Some((start, bg)), _) => {
                            push_bg_quad(quads, scale, cell_w_px, cell_h_px, start, col, y, bg);
                            run = bg_opt.map(|b| (col, b));
                        }
                        (None, Some(b)) => {
                            run = Some((col, b));
                        }
                        (None, None) => {}
                    }
                }
                if let Some((start, bg)) = run {
                    push_bg_quad(quads, scale, cell_w_px, cell_h_px, start, row.len(), y, bg);
                }
            }
            // Selection highlight: drawn after cell-bg runs so it sits on top
            // when the underlying cells had a non-default bg. Uses the
            // terminal's default fg as the selection color so the inverted
            // text (rendered with cell.bg) reads against it.
            if let Some(sel) = snap.selection {
                for row_idx in sel.start_line..=sel.end_line.min(snap.cells.len().saturating_sub(1))
                {
                    let row = &snap.cells[row_idx];
                    let row_len = row.len();
                    if row_len == 0 {
                        continue;
                    }
                    let (s, e) = if sel.is_block {
                        (sel.start_col, sel.end_col)
                    } else if sel.start_line == sel.end_line {
                        (sel.start_col, sel.end_col)
                    } else if row_idx == sel.start_line {
                        (sel.start_col, row_len - 1)
                    } else if row_idx == sel.end_line {
                        (0, sel.end_col)
                    } else {
                        (0, row_len - 1)
                    };
                    let s = s.min(row_len - 1);
                    let e = e.min(row_len - 1);
                    let y = top_offset_px + row_idx as f32 * cell_h_px;
                    push_bg_quad(quads, scale, cell_w_px, cell_h_px, s, e + 1, y, snap.fg);
                }
            }
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

            // Grow the row-buffer pool as needed; reuse what we have.
            while self.row_buffers.len() < snap.cells.len() {
                self.row_buffers
                    .push(Buffer::new(&mut self.font_system, metrics));
            }
            for (row_idx, row) in snap.cells.iter().enumerate() {
                let buf = &mut self.row_buffers[row_idx];
                buf.set_metrics(&mut self.font_system, metrics);
                buf.set_size(
                    &mut self.font_system,
                    Some(cell_w_px * row.len() as f32 + cell_w_px),
                    Some(cell_h_px + 2.0),
                );
                let (text, spans_meta) =
                    build_row_text(row, row_idx, snap, family_of(&family_name));
                let spans = spans_meta
                    .iter()
                    .map(|s| (&text[s.range.clone()], s.attrs.clone()));
                buf.set_rich_text(
                    &mut self.font_system,
                    spans,
                    &Attrs::new().family(family_of(&family_name)),
                    Shaping::Advanced,
                    None,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
            }
            row_count = snap.cells.len();
        }

        // ===== Build TextAreas borrowing from the cached buffers. =====
        let bounds = TextBounds {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        let tab_area = self.tab_buffer.as_ref().map(|buf| TextArea {
            buffer: buf,
            left: tab_pos.0,
            top: tab_pos.1,
            scale: 1.0,
            bounds,
            default_color: tab_pos.2,
            custom_glyphs: &[],
        });
        let row_areas = (0..row_count).map(|i| {
            let y = top_offset_px + i as f32 * cell_h_px;
            TextArea {
                buffer: &self.row_buffers[i],
                left: PAD_X * scale,
                top: y,
                scale: 1.0,
                bounds,
                default_color: default_fg,
                custom_glyphs: &[],
            }
        });
        let text_areas: Vec<TextArea<'_>> = tab_area.into_iter().chain(row_areas).collect();

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

        self.quads.upload(&self.device, &self.queue, width, height, &self.quad_scratch);

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
                        load: wgpu::LoadOp::Clear(self.clear_color),
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
