use std::sync::Arc;

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use winit::window::Window;

use crate::config::Colors as ConfigColors;
use crate::quad::{Quad, QuadPipeline};
use crate::terminal::{GridSnapshot, TerminalSession, UrlMatch, UrlSpan};

struct TabBarTheme {
    /// Background of the strip behind all tabs (linear-space wgpu color).
    bar_bg: [f32; 4],
    /// Background of the active tab — matches the terminal content bg so the
    /// active tab visually merges into the page.
    active_bg: [f32; 4],
    /// Thin accent stripe along the bottom of the active tab.
    accent: [f32; 4],
    /// 1px separator drawn between adjacent inactive tabs.
    separator: [f32; 4],
    /// Text color for the active tab (sRGB; glyphon uses sRGB).
    active_fg: [u8; 3],
    /// Text color for inactive tabs.
    inactive_fg: [u8; 3],
}

impl TabBarTheme {
    fn derive(colors: &ConfigColors) -> Self {
        Self {
            // A bit darker than before (0.55 vs 0.7) so the active tab visibly
            // "lifts" off the bar without needing per-tab borders.
            bar_bg: linear_rgba(darken(colors.background, 0.55)),
            active_bg: linear_rgba(colors.background),
            accent: linear_rgba(colors.bright.blue),
            separator: linear_rgba(darken(colors.background, 0.85)),
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

#[allow(clippy::too_many_arguments)]
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
    // Linearize: the surface is sRGB, so the fragment shader output is
    // treated as linear and gamma-corrected on store. Passing raw
    // sRGB/255 values made cell backgrounds visibly over-saturated.
    quads.push(Quad {
        rect: [x, y_px, w, cell_h_px],
        color: linear_rgba(bg),
    });
}

#[allow(clippy::too_many_arguments)]
fn push_underline_quad(
    quads: &mut Vec<Quad>,
    scale: f32,
    cell_w_px: f32,
    h_px: f32,
    start_col: usize,
    end_col: usize,
    y_px: f32,
    fg: [u8; 3],
) {
    let x = PAD_X * scale + start_col as f32 * cell_w_px;
    let w = (end_col - start_col) as f32 * cell_w_px;
    quads.push(Quad {
        rect: [x, y_px, w, h_px],
        color: linear_rgba(fg),
    });
}

fn hover_url_covers(spans: &[UrlSpan], row: usize, col: usize) -> bool {
    spans
        .iter()
        .any(|s| s.line == row && col >= s.start_col && col <= s.end_col)
}

fn cell_in_selection(snap: &GridSnapshot, row: usize, col: usize) -> bool {
    let Some(sel) = snap.selection else {
        return false;
    };
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
            // The selection highlight quad is drawn in snap.fg, so always
            // render selected text in snap.bg for legibility — regardless of
            // the cell's own bg (which may be a syntax-highlight color, etc.).
            snap.bg
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
        spans.push(RowSpan {
            range: start..end,
            attrs,
        });
    }
    (text, spans)
}

fn measure_cell_width(
    font_system: &mut FontSystem,
    font_size: f32,
    line_height: f32,
    family_name: &str,
) -> f32 {
    let family = if family_name.eq_ignore_ascii_case("monospace") {
        Family::Monospace
    } else {
        Family::Name(family_name)
    };
    let measure = |fs: &mut FontSystem, text: &str| -> f32 {
        let metrics = Metrics::new(font_size, line_height);
        let mut buf = Buffer::new(fs, metrics);
        buf.set_size(fs, Some(1000.0), Some(line_height + 4.0));
        buf.set_text(
            fs,
            text,
            &Attrs::new().family(family),
            Shaping::Advanced,
            None,
        );
        buf.shape_until_scroll(fs, false);
        buf.layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .map(|g| g.x + g.w)
            .fold(0.0_f32, f32::max)
    };

    let m_width = measure(font_system, "MMMMMMMMMM");
    let cell = if m_width > 0.0 {
        m_width / 10.0
    } else {
        font_size * 0.6
    };

    // Sanity check: a proportional font will measure 'i' and 'W' at very
    // different widths, and the grid will look wrong. Warn so the user knows
    // why their terminal looks off.
    let i_width = measure(font_system, "iiiiiiiiii");
    let w_width = measure(font_system, "WWWWWWWWWW");
    if i_width > 0.0 && w_width > 0.0 {
        let ratio = w_width / i_width;
        if ratio > 1.15 {
            log::warn!(
                "font {family_name:?} appears to be proportional (W/i width ratio = {ratio:.2}); \
                 the grid will not align correctly. Use a monospace font."
            );
        }
    }
    cell
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
    /// Reusable buffer for the bottom URL preview text. Allocated lazily the
    /// first time a hover URL is shown.
    url_bar_buffer: Option<Buffer>,
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
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
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
                label: Some("aterm device"),
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
            url_bar_buffer: None,
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

    /// Update font size and recompute the logical cell width. The cached
    /// per-row text buffers are dropped so the next render rebuilds them
    /// with the new metrics — re-shaping all rows on every zoom keystroke
    /// is fine because they'd otherwise stay at the old size.
    pub fn set_font_metrics(&mut self, font_size: f32, line_height: f32) {
        self.font_size = font_size;
        self.line_height = line_height;
        self.cell_width_logical = measure_cell_width(
            &mut self.font_system,
            font_size,
            line_height,
            &self.font_family,
        );
        self.row_buffers.clear();
        self.tab_buffer = None;
        self.url_bar_buffer = None;
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

    /// Build the tab strip: per-tab text segments, hit regions, and the
    /// active-tab background quad. Returns the (x, baseline_y, default_color)
    /// at which the tab text buffer should be drawn.
    fn prepare_tab_bar(
        &mut self,
        tabs: &[TerminalSession],
        active_idx: usize,
        tab_bar_height: f32,
        left_inset: f32,
        metrics: Metrics,
    ) -> (f32, f32, Color) {
        let width = self.surface_config.width;
        let scale = self.window.scale_factor() as f32;
        let cell_w_px = self.cell_width_logical * scale;
        // Left edge of the tab strip in physical pixels. On macOS this is
        // shifted right to clear the traffic-light buttons; elsewhere it's
        // just PAD_X.
        let strip_left_px = (PAD_X + left_inset) * scale;

        // Variable-width tabs: each tab gets either its natural width
        // (title length in cells) or a fair share of what's available,
        // whichever is smaller. Slack from short titles is redistributed
        // round-robin to tabs that still want more room.
        const SEP_CHARS: usize = 2;
        const MIN_TITLE_CHARS: usize = 4;
        let usable_chars = ((width as f32 - strip_left_px - PAD_X * scale) / cell_w_px)
            .floor()
            .max(0.0) as usize;

        let mut budgets: Vec<usize> = Vec::with_capacity(tabs.len());
        if !tabs.is_empty() {
            let total_seps = tabs.len().saturating_sub(1) * SEP_CHARS;
            let mut pool = usable_chars.saturating_sub(total_seps);
            let natural: Vec<usize> = tabs.iter().map(|t| t.title().chars().count()).collect();
            // Start by giving each tab MIN_TITLE_CHARS (or its natural width
            // if smaller). This guarantees a usable rendering even when the
            // bar is very narrow.
            for &n in &natural {
                let initial = n.min(MIN_TITLE_CHARS);
                let take = initial.min(pool);
                budgets.push(take);
                pool -= take;
            }
            // Fill any remaining want round-robin until the pool is empty or
            // no tab wants more.
            loop {
                if pool == 0 {
                    break;
                }
                let mut progressed = false;
                for i in 0..budgets.len() {
                    if pool == 0 {
                        break;
                    }
                    if budgets[i] < natural[i] {
                        budgets[i] += 1;
                        pool -= 1;
                        progressed = true;
                    }
                }
                if !progressed {
                    break;
                }
            }
            // Any unclaimed budget (every tab already at natural width) goes
            // back to padding around each title — but cap so we don't end up
            // with absurd amounts of whitespace on a near-empty bar.
        }

        self.tab_hit_regions.clear();
        let bar_h_px = tab_bar_height * scale;
        let accent_h_px = (2.0 * scale).round().max(1.0);
        let sep_w_px = (scale).round().max(1.0);
        let mut segments: Vec<(String, bool)> = Vec::new();
        let mut tab_text = String::new();
        let mut chars_cursor: usize = 0;
        for (i, t) in tabs.iter().enumerate() {
            if i > 0 {
                let sep_pad = " ".repeat(SEP_CHARS);
                segments.push((sep_pad.clone(), false));
                tab_text.push_str(&sep_pad);
                let sep_mid_x =
                    strip_left_px + (chars_cursor as f32 + SEP_CHARS as f32 * 0.5) * cell_w_px;
                let prev_active = i - 1 == active_idx;
                let next_active = i == active_idx;
                // Only draw the dividing line between two inactive tabs.
                if !prev_active && !next_active {
                    let inset_y = (bar_h_px * 0.25).round();
                    self.quad_scratch.push(Quad {
                        rect: [
                            sep_mid_x - sep_w_px * 0.5,
                            inset_y,
                            sep_w_px,
                            bar_h_px - 2.0 * inset_y,
                        ],
                        color: self.tab_theme.separator,
                    });
                }
                chars_cursor += SEP_CHARS;
            }
            let title = truncate_with_ellipsis(t.title(), budgets.get(i).copied().unwrap_or(0));
            let title_chars = title.chars().count();
            let x0 = strip_left_px + chars_cursor as f32 * cell_w_px;
            let x1 = x0 + title_chars as f32 * cell_w_px;
            self.tab_hit_regions.push((i, x0 - 4.0, x1 + 4.0));
            if i == active_idx {
                // Background quad merges the active tab into the terminal
                // page below.
                self.quad_scratch.push(Quad {
                    rect: [x0 - 4.0, 0.0, (x1 - x0) + 8.0, bar_h_px],
                    color: self.tab_theme.active_bg,
                });
                // Accent stripe along the bottom of the active tab.
                self.quad_scratch.push(Quad {
                    rect: [
                        x0 - 4.0,
                        bar_h_px - accent_h_px,
                        (x1 - x0) + 8.0,
                        accent_h_px,
                    ],
                    color: self.tab_theme.accent,
                });
            }
            tab_text.push_str(&title);
            chars_cursor += title_chars;
            segments.push((title, i == active_idx));
        }
        if segments.is_empty() {
            segments.push(("(no tabs)".into(), false));
        }

        let buf = self
            .tab_buffer
            .get_or_insert_with(|| Buffer::new(&mut self.font_system, metrics));
        buf.set_metrics(&mut self.font_system, metrics);
        buf.set_size(&mut self.font_system, Some(width as f32), Some(bar_h_px));
        let family = family_of(&self.font_family);
        let active_fg = self.tab_theme.active_fg;
        let inactive_fg = self.tab_theme.inactive_fg;
        let spans = segments.iter().map(|(text, active)| {
            let fg = if *active { active_fg } else { inactive_fg };
            let mut attrs = Attrs::new()
                .family(family)
                .color(Color::rgb(fg[0], fg[1], fg[2]));
            if *active {
                attrs = attrs.weight(glyphon::Weight::BOLD);
            }
            (text.as_str(), attrs)
        });
        buf.set_rich_text(
            &mut self.font_system,
            spans,
            &Attrs::new().family(family),
            Shaping::Advanced,
            None,
        );
        buf.shape_until_scroll(&mut self.font_system, false);

        (
            strip_left_px,
            (tab_bar_height - self.line_height) * 0.5 * scale,
            Color::rgb(inactive_fg[0], inactive_fg[1], inactive_fg[2]),
        )
    }

    /// Push the URL preview bar's background quad and prepare its text buffer.
    /// Mirrors alacritty's hint-mode UX: while a URL is hovered we show the
    /// full URI in a strip near the bottom of the window. Insets on the sides
    /// and bottom keep the bar clear of the macOS rounded window corners and
    /// any system shadow on the bottom edge.
    fn prepare_url_bar(
        &mut self,
        hover_url: Option<&UrlMatch>,
        width: u32,
        height: u32,
        metrics: Metrics,
    ) -> Option<(f32, f32, Color)> {
        let url = hover_url?;
        let scale = self.window.scale_factor() as f32;
        let cell_w_px = self.cell_width_logical * scale;
        let cell_h_px = self.line_height * scale;

        let bar_inset_x = 12.0 * scale;
        let bar_inset_bottom = 8.0 * scale;
        let bar_pad_x = 8.0 * scale;
        let bar_pad_y = 3.0 * scale;
        let bar_h_px = cell_h_px + 2.0 * bar_pad_y;
        let bar_w_px = (width as f32 - 2.0 * bar_inset_x).max(0.0);
        let bar_x = bar_inset_x;
        let bar_y = (height as f32 - bar_h_px - bar_inset_bottom).max(0.0);
        let usable_chars = ((bar_w_px - 2.0 * bar_pad_x) / cell_w_px).floor().max(1.0) as usize;
        let display = truncate_with_ellipsis(&url.uri, usable_chars);

        self.quad_scratch.push(Quad {
            rect: [bar_x, bar_y, bar_w_px, bar_h_px],
            color: self.tab_theme.bar_bg,
        });

        let buf = self
            .url_bar_buffer
            .get_or_insert_with(|| Buffer::new(&mut self.font_system, metrics));
        buf.set_metrics(&mut self.font_system, metrics);
        buf.set_size(&mut self.font_system, Some(bar_w_px), Some(bar_h_px));
        let fg = self.tab_theme.active_fg;
        let family = family_of(&self.font_family);
        buf.set_text(
            &mut self.font_system,
            &display,
            &Attrs::new()
                .family(family)
                .color(Color::rgb(fg[0], fg[1], fg[2])),
            Shaping::Advanced,
            None,
        );
        buf.shape_until_scroll(&mut self.font_system, false);

        Some((
            bar_x + bar_pad_x,
            bar_y + bar_pad_y,
            Color::rgb(fg[0], fg[1], fg[2]),
        ))
    }

    pub fn render(
        &mut self,
        active: Option<&TerminalSession>,
        tabs: &[TerminalSession],
        active_idx: usize,
        tab_bar_height: f32,
        tab_bar_left_inset: f32,
        hover_url: Option<&UrlMatch>,
    ) -> Result<(), String> {
        let width = self.surface_config.width;
        let height = self.surface_config.height;
        self.viewport
            .update(&self.queue, Resolution { width, height });

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

        self.quad_scratch.clear();
        self.quad_scratch.push(Quad {
            rect: [0.0, 0.0, width as f32, tab_bar_height * scale],
            color: self.tab_theme.bar_bg,
        });

        let tab_pos = self.prepare_tab_bar(
            tabs,
            active_idx,
            tab_bar_height,
            tab_bar_left_inset,
            metrics,
        );
        let family_name = self.font_family.clone();
        let quads = &mut self.quad_scratch;

        // ===== Grid: background quads, cursor quad, row text buffers. =====
        let top_offset_px = (tab_bar_height + PAD_Y) * scale;
        let mut row_count = 0usize;
        if let Some(snap) = snapshot.as_ref() {
            let default_bg = snap.bg;
            for (row_idx, row) in snap.cells.iter().enumerate() {
                let y = top_offset_px + row_idx as f32 * cell_h_px;
                let cursor_col =
                    (snap.cursor_visible && snap.cursor_line == row_idx).then_some(snap.cursor_col);
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
                    let last = row_len.saturating_sub(1);
                    let (s, e) = if sel.is_block || sel.start_line == sel.end_line {
                        (sel.start_col, sel.end_col)
                    } else if row_idx == sel.start_line {
                        (sel.start_col, last)
                    } else if row_idx == sel.end_line {
                        (0, sel.end_col)
                    } else {
                        (0, last)
                    };
                    let s = s.min(last);
                    let e = e.min(last);
                    let y = top_offset_px + row_idx as f32 * cell_h_px;
                    push_bg_quad(quads, scale, cell_w_px, cell_h_px, s, e + 1, y, snap.fg);
                }
            }
            if snap.cursor_visible {
                let x = PAD_X * scale + snap.cursor_col as f32 * cell_w_px;
                let y = top_offset_px + snap.cursor_line as f32 * cell_h_px;
                quads.push(Quad {
                    rect: [x, y, cell_w_px, cell_h_px],
                    color: linear_rgba(snap.fg),
                });
            }

            // Underlines: SGR underline or OSC 8 hyperlink cells are drawn
            // permanently; hovered plain-text URLs add to the same set. We
            // collect (line, col_start, col_end_exclusive) ranges and emit
            // one thin quad per range so wide underlines are still one draw.
            let hover_spans = hover_url.map(|u| u.spans.as_slice()).unwrap_or(&[]);
            let underline_h_px = (scale.round()).max(1.0);
            let underline_y_off = cell_h_px - underline_h_px;
            for (row_idx, row) in snap.cells.iter().enumerate() {
                let mut start: Option<usize> = None;
                for (col, cell) in row.iter().enumerate() {
                    let on = cell.underline || hover_url_covers(hover_spans, row_idx, col);
                    match (start, on) {
                        (None, true) => start = Some(col),
                        (Some(s), false) => {
                            push_underline_quad(
                                quads,
                                scale,
                                cell_w_px,
                                underline_h_px,
                                s,
                                col,
                                top_offset_px + row_idx as f32 * cell_h_px + underline_y_off,
                                snap.fg,
                            );
                            start = None;
                        }
                        _ => {}
                    }
                }
                if let Some(s) = start {
                    push_underline_quad(
                        quads,
                        scale,
                        cell_w_px,
                        underline_h_px,
                        s,
                        row.len(),
                        top_offset_px + row_idx as f32 * cell_h_px + underline_y_off,
                        snap.fg,
                    );
                }
            }

            // Grow the row-buffer pool as needed; reuse what we have. Reserve
            // up-front so a large jump (e.g. window maximize) doesn't trigger
            // repeated Vec reallocations.
            if self.row_buffers.len() < snap.cells.len() {
                let extra = snap.cells.len() - self.row_buffers.len();
                self.row_buffers.reserve_exact(extra);
                for _ in 0..extra {
                    self.row_buffers
                        .push(Buffer::new(&mut self.font_system, metrics));
                }
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

        // ===== Bottom URL preview bar. =====
        let url_bar_pos = self.prepare_url_bar(hover_url, width, height, metrics);

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
        let url_bar_area = url_bar_pos.and_then(|(x, y, color)| {
            self.url_bar_buffer.as_ref().map(|buf| TextArea {
                buffer: buf,
                left: x,
                top: y,
                scale: 1.0,
                bounds,
                default_color: color,
                custom_glyphs: &[],
            })
        });
        let text_areas: Vec<TextArea<'_>> = tab_area
            .into_iter()
            .chain(row_areas)
            .chain(url_bar_area)
            .collect();

        if let Err(e) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.text_atlas,
            &self.viewport,
            text_areas,
            &mut self.swash_cache,
        ) {
            // PrepareError is non-fatal (typically "atlas full" when the
            // viewport is enormous). Skip this frame and try again.
            log::warn!("text prepare failed: {e:?}");
            return Ok(());
        }

        self.quads
            .upload(&self.device, &self.queue, width, height, &self.quad_scratch);

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
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
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
            if let Err(e) = self
                .text_renderer
                .render(&self.text_atlas, &self.viewport, &mut pass)
            {
                log::warn!("text render failed: {e:?}");
            }
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        self.text_atlas.trim();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{GridSnapshot, SelectionView, SnapCell};

    #[test]
    fn truncate_keeps_short_strings_intact() {
        assert_eq!(truncate_with_ellipsis("abc", 10), "abc");
        assert_eq!(truncate_with_ellipsis("abc", 3), "abc");
    }

    #[test]
    fn truncate_appends_ellipsis_when_over_budget() {
        assert_eq!(truncate_with_ellipsis("abcdef", 4), "abc…");
        assert_eq!(truncate_with_ellipsis("abcdef", 1), "…");
        assert_eq!(truncate_with_ellipsis("abcdef", 0), "");
    }

    #[test]
    fn truncate_handles_multibyte_chars() {
        // chars(), not bytes — 4 chars of "αβγδεζ" should fit in budget 4.
        assert_eq!(truncate_with_ellipsis("αβγδεζ", 4), "αβγ…");
    }

    fn snap_with_selection(sel: SelectionView) -> GridSnapshot {
        GridSnapshot {
            cells: vec![vec![SnapCell::default(); 10]; 5],
            cursor_line: 0,
            cursor_col: 0,
            cursor_visible: false,
            fg: [0; 3],
            bg: [0; 3],
            selection: Some(sel),
        }
    }

    #[test]
    fn cell_in_selection_single_line() {
        let snap = snap_with_selection(SelectionView {
            start_line: 1,
            start_col: 2,
            end_line: 1,
            end_col: 5,
            is_block: false,
        });
        assert!(!cell_in_selection(&snap, 1, 1));
        assert!(cell_in_selection(&snap, 1, 2));
        assert!(cell_in_selection(&snap, 1, 5));
        assert!(!cell_in_selection(&snap, 1, 6));
        assert!(!cell_in_selection(&snap, 0, 3));
    }

    #[test]
    fn cell_in_selection_multi_line() {
        let snap = snap_with_selection(SelectionView {
            start_line: 1,
            start_col: 3,
            end_line: 3,
            end_col: 2,
            is_block: false,
        });
        // Row 1: from start_col to end of row.
        assert!(!cell_in_selection(&snap, 1, 2));
        assert!(cell_in_selection(&snap, 1, 3));
        assert!(cell_in_selection(&snap, 1, 9));
        // Middle row: entirely selected.
        assert!(cell_in_selection(&snap, 2, 0));
        assert!(cell_in_selection(&snap, 2, 9));
        // Last row: up to end_col.
        assert!(cell_in_selection(&snap, 3, 0));
        assert!(cell_in_selection(&snap, 3, 2));
        assert!(!cell_in_selection(&snap, 3, 3));
    }

    #[test]
    fn hover_url_covers_matches_only_inside_spans() {
        let spans = vec![
            UrlSpan {
                line: 1,
                start_col: 2,
                end_col: 5,
            },
            UrlSpan {
                line: 3,
                start_col: 0,
                end_col: 9,
            },
        ];
        assert!(!hover_url_covers(&spans, 0, 3));
        assert!(!hover_url_covers(&spans, 1, 1));
        assert!(hover_url_covers(&spans, 1, 2));
        assert!(hover_url_covers(&spans, 1, 5));
        assert!(!hover_url_covers(&spans, 1, 6));
        assert!(hover_url_covers(&spans, 3, 0));
        assert!(hover_url_covers(&spans, 3, 9));
        assert!(!hover_url_covers(&spans, 4, 0));
    }

    #[test]
    fn cell_in_selection_block() {
        let snap = snap_with_selection(SelectionView {
            start_line: 0,
            start_col: 2,
            end_line: 2,
            end_col: 4,
            is_block: true,
        });
        assert!(cell_in_selection(&snap, 0, 2));
        assert!(cell_in_selection(&snap, 2, 4));
        assert!(!cell_in_selection(&snap, 1, 1));
        assert!(!cell_in_selection(&snap, 1, 5));
        assert!(!cell_in_selection(&snap, 3, 3));
    }
}
