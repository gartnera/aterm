use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyLoop, Msg, Notifier};

use winit::event_loop::EventLoopProxy;

use crate::config::Colors as ConfigColors;
use crate::WakeEvent;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{point_to_viewport, viewport_to_point, TermMode};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use alacritty_terminal::Term;
use crossbeam_channel::{Receiver, Sender, unbounded};

#[derive(Clone)]
pub struct ChannelListener {
    tx: Sender<TermEvent>,
    proxy: EventLoopProxy<WakeEvent>,
}

#[derive(Clone, Copy, Default)]
pub struct SnapCell {
    pub ch: char,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub bold: bool,
    pub italic: bool,
}

pub struct GridSnapshot {
    pub cells: Vec<Vec<SnapCell>>,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub cursor_visible: bool,
    pub fg: [u8; 3],
    /// Terminal's default background — cells with this bg are not drawn so
    /// they fall through to the surface clear color.
    pub bg: [u8; 3],
    /// Active selection translated to viewport coordinates, if any. `end` is
    /// inclusive. For non-block selections, rows between start.line and
    /// end.line are fully selected across their entire width.
    pub selection: Option<SelectionView>,
}

#[derive(Clone, Copy)]
pub struct SelectionView {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub is_block: bool,
}

enum Role {
    Fg,
    Bg,
}

fn rgb_to_arr(rgb: Rgb) -> [u8; 3] {
    [rgb.r, rgb.g, rgb.b]
}

fn dim([r, g, b]: [u8; 3]) -> [u8; 3] {
    // SGR 2 (dim) renders at ~66% intensity. Matches alacritty's fallback
    // when no explicit dim color is configured.
    [
        (r as f32 * 0.66).round() as u8,
        (g as f32 * 0.66).round() as u8,
        (b as f32 * 0.66).round() as u8,
    ]
}

fn named_from_palette(name: NamedColor, palette: &ConfigColors) -> Option<[u8; 3]> {
    use NamedColor as N;
    Some(match name {
        N::Foreground => palette.foreground,
        N::Background => palette.background,
        N::Cursor => palette.cursor,
        N::Black => palette.normal.black,
        N::Red => palette.normal.red,
        N::Green => palette.normal.green,
        N::Yellow => palette.normal.yellow,
        N::Blue => palette.normal.blue,
        N::Magenta => palette.normal.magenta,
        N::Cyan => palette.normal.cyan,
        N::White => palette.normal.white,
        N::BrightBlack => palette.bright.black,
        N::BrightRed => palette.bright.red,
        N::BrightGreen => palette.bright.green,
        N::BrightYellow => palette.bright.yellow,
        N::BrightBlue => palette.bright.blue,
        N::BrightMagenta => palette.bright.magenta,
        N::BrightCyan => palette.bright.cyan,
        N::BrightWhite => palette.bright.white,
        N::BrightForeground => palette.foreground,
        N::DimForeground => dim(palette.foreground),
        N::DimBlack => dim(palette.normal.black),
        N::DimRed => dim(palette.normal.red),
        N::DimGreen => dim(palette.normal.green),
        N::DimYellow => dim(palette.normal.yellow),
        N::DimBlue => dim(palette.normal.blue),
        N::DimMagenta => dim(palette.normal.magenta),
        N::DimCyan => dim(palette.normal.cyan),
        N::DimWhite => dim(palette.normal.white),
    })
}

fn indexed_from_palette(idx: u8, palette: &ConfigColors) -> [u8; 3] {
    // 0..16: configured normal / bright palette.
    if idx < 16 {
        let bright = idx >= 8;
        let pal = if bright { &palette.bright } else { &palette.normal };
        return match idx % 8 {
            0 => pal.black,
            1 => pal.red,
            2 => pal.green,
            3 => pal.yellow,
            4 => pal.blue,
            5 => pal.magenta,
            6 => pal.cyan,
            _ => pal.white,
        };
    }
    // 16..232: 6x6x6 colour cube.
    if (16..=231).contains(&idx) {
        let n = idx - 16;
        let r = n / 36;
        let g = (n / 6) % 6;
        let b = n % 6;
        let scale = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
        return [scale(r), scale(g), scale(b)];
    }
    // 232..256: 24-step greyscale ramp.
    let gray = 8 + 10 * (idx - 232);
    [gray, gray, gray]
}

fn resolve_color(
    color: AnsiColor,
    colors: &alacritty_terminal::term::color::Colors,
    palette: &ConfigColors,
    role: Role,
) -> [u8; 3] {
    match color {
        AnsiColor::Named(name) => match colors[name] {
            Some(rgb) => rgb_to_arr(rgb),
            None => named_from_palette(name, palette).unwrap_or(match role {
                Role::Fg => palette.foreground,
                Role::Bg => palette.background,
            }),
        },
        AnsiColor::Spec(rgb) => rgb_to_arr(rgb),
        AnsiColor::Indexed(idx) => match colors[idx as usize] {
            Some(rgb) => rgb_to_arr(rgb),
            None => indexed_from_palette(idx, palette),
        },
    }
}

impl EventListener for ChannelListener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.tx.send(event);
        let _ = self.proxy.send_event(WakeEvent);
    }
}

pub struct TerminalSession {
    term: Arc<FairMutex<Term<ChannelListener>>>,
    notifier: Notifier,
    events: Receiver<TermEvent>,
    title: String,
    cols: u16,
    lines: u16,
    exited: bool,
    palette: ConfigColors,
}

impl TerminalSession {
    pub fn spawn(
        cols: u16,
        lines: u16,
        cell_width: u16,
        cell_height: u16,
        proxy: EventLoopProxy<WakeEvent>,
        palette: ConfigColors,
    ) -> std::io::Result<Self> {
        let window_size = WindowSize {
            num_lines: lines,
            num_cols: cols,
            cell_width,
            cell_height,
        };
        let term_size = TermSize::new(cols as usize, lines as usize);

        let mut pty_options = PtyOptions::default();
        // On macOS, alacritty defaults to `/usr/bin/login`, which the harness
        // sandbox blocks. Explicitly use $SHELL (with a /bin/zsh fallback) so
        // we spawn the shell directly without `login`.
        let shell_path =
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        pty_options.shell = Some(Shell::new(shell_path, Vec::new()));
        // Launchd-spawned processes (Finder/Spotlight/.app) inherit an empty
        // TERM, leaving ncurses programs unable to initialize. Set it here so
        // the shell works regardless of how aterm itself was launched.
        pty_options.env.insert("TERM".into(), "xterm-256color".into());
        pty_options.env.insert("COLORTERM".into(), "truecolor".into());
        // Launchd starts the .app in `/`. If our cwd looks like a launchd
        // default, start the shell in $HOME instead. When aterm was launched
        // from a shell in a real directory, inherit that cwd as usual.
        if std::env::current_dir().map(|p| p == std::path::Path::new("/")).unwrap_or(false) {
            if let Some(home) = dirs::home_dir() {
                pty_options.working_directory = Some(home);
            }
        }
        // window_id is opaque metadata used by alacritty's PTY layer.
        let pty = tty::new(&pty_options, window_size, 0)?;

        let (events_tx, events_rx) = unbounded();
        let listener = ChannelListener { tx: events_tx, proxy };

        let term_config = TermConfig::default();
        let term = Term::new(term_config, &term_size, listener.clone());
        let term = Arc::new(FairMutex::new(term));

        let pty_loop = PtyLoop::new(term.clone(), listener, pty, pty_options.drain_on_exit, false)?;
        let notifier = Notifier(pty_loop.channel());
        let _io_thread = pty_loop.spawn();

        Ok(Self {
            term,
            notifier,
            events: events_rx,
            title: "shell".into(),
            cols,
            lines,
            exited: false,
            palette,
        })
    }

    pub fn is_exited(&self) -> bool {
        self.exited
    }

    /// Whether the terminal is in DECCKM (application cursor) mode. Used by
    /// the keyboard mapper to choose between CSI and SS3 arrow sequences.
    pub fn app_cursor_mode(&self) -> bool {
        self.term.lock().mode().contains(TermMode::APP_CURSOR)
    }

    /// Whether the application has enabled bracketed paste (DEC private 2004).
    /// When true, pasted text should be wrapped in `\x1b[200~ … \x1b[201~`.
    pub fn bracketed_paste(&self) -> bool {
        self.term.lock().mode().contains(TermMode::BRACKETED_PASTE)
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn send_input<B: Into<std::borrow::Cow<'static, [u8]>>>(&self, bytes: B) {
        self.notifier.notify(bytes);
    }

    /// Scroll the viewport. Use `Scroll::Bottom` to snap back to live output.
    pub fn scroll(&self, scroll: Scroll) {
        self.term.lock().scroll_display(scroll);
    }

    /// Begin a new selection at the given viewport cell.
    pub fn selection_start(&self, vp_line: usize, vp_col: usize, right_half: bool) {
        let mut term = self.term.lock();
        let lines = term.screen_lines();
        let cols = term.columns();
        let vp_line = vp_line.min(lines.saturating_sub(1));
        let vp_col = vp_col.min(cols.saturating_sub(1));
        let display_offset = term.grid().display_offset();
        let point = viewport_to_point(
            display_offset,
            Point::new(vp_line, Column(vp_col)),
        );
        let side = if right_half { Side::Right } else { Side::Left };
        term.selection = Some(Selection::new(SelectionType::Simple, point, side));
    }

    /// Extend the in-progress selection to the given viewport cell.
    pub fn selection_update(&self, vp_line: usize, vp_col: usize, right_half: bool) {
        let mut term = self.term.lock();
        let lines = term.screen_lines();
        let cols = term.columns();
        let vp_line = vp_line.min(lines.saturating_sub(1));
        let vp_col = vp_col.min(cols.saturating_sub(1));
        let display_offset = term.grid().display_offset();
        let point = viewport_to_point(
            display_offset,
            Point::new(vp_line, Column(vp_col)),
        );
        let side = if right_half { Side::Right } else { Side::Left };
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string().filter(|s| !s.is_empty())
    }

    pub fn resize(&mut self, cols: u16, lines: u16, cell_width: u16, cell_height: u16) {
        self.cols = cols;
        self.lines = lines;
        let window_size = WindowSize {
            num_lines: lines,
            num_cols: cols,
            cell_width,
            cell_height,
        };
        let size = TermSize::new(cols as usize, lines as usize);
        let _ = self.notifier.0.send(Msg::Resize(window_size));
        self.term.lock().resize(size);
    }

    /// Snapshot the current viewport for rendering. Takes the lock briefly,
    /// then releases it so the renderer can run unsynchronized.
    pub fn snapshot(&self) -> GridSnapshot {
        let term = self.term.lock();
        let content = term.renderable_content();
        let display_offset = content.display_offset;
        let cols = term.columns();
        let lines = term.screen_lines();
        let cursor_point = content.cursor.point;
        let cursor_visible = !matches!(content.cursor.shape, CursorShape::Hidden);
        let selection = content.selection.and_then(|range| {
            // Convert to viewport coordinates and clamp to the visible grid so
            // selections that extend into scrollback off-screen render as a
            // partial highlight rather than disappearing.
            let start_line = range.start.line.0 + display_offset as i32;
            let end_line = range.end.line.0 + display_offset as i32;
            if end_line < 0 || start_line >= lines as i32 {
                return None;
            }
            let start_line = start_line.max(0) as usize;
            let end_line = end_line.min(lines as i32 - 1) as usize;
            Some(SelectionView {
                start_line,
                start_col: range.start.column.0.min(cols.saturating_sub(1)),
                end_line,
                end_col: range.end.column.0.min(cols.saturating_sub(1)),
                is_block: range.is_block,
            })
        });

        let mut cells: Vec<Vec<SnapCell>> = (0..lines)
            .map(|_| {
                (0..cols)
                    .map(|_| SnapCell::default())
                    .collect()
            })
            .collect();

        for indexed in content.display_iter {
            let Some(vp) = point_to_viewport(display_offset, indexed.point) else {
                continue;
            };
            if vp.line >= lines || vp.column.0 >= cols {
                continue;
            }
            let cell = &indexed.cell;
            let mut fg = resolve_color(cell.fg, content.colors, &self.palette, Role::Fg);
            let mut bg = resolve_color(cell.bg, content.colors, &self.palette, Role::Bg);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let ch = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                '\0'
            } else {
                cell.c
            };
            cells[vp.line][vp.column.0] = SnapCell {
                ch,
                fg,
                bg,
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
            };
        }
        let cursor_vp = point_to_viewport(display_offset, cursor_point);
        GridSnapshot {
            cells,
            cursor_line: cursor_vp.map(|p| p.line).unwrap_or(0),
            cursor_col: cursor_vp.map(|p| p.column.0).unwrap_or(0),
            cursor_visible,
            fg: content.colors[NamedColor::Foreground]
                .map(rgb_to_arr)
                .unwrap_or(self.palette.foreground),
            bg: content.colors[NamedColor::Background]
                .map(rgb_to_arr)
                .unwrap_or(self.palette.background),
            selection,
        }
    }

    /// Drain any pending alacritty events. Returns whether a redraw is needed.
    pub fn pump_events(&mut self) -> bool {
        let mut wake = false;
        while let Ok(event) = self.events.try_recv() {
            match event {
                TermEvent::Title(t) => self.title = t,
                TermEvent::ResetTitle => self.title = "shell".into(),
                TermEvent::Wakeup => wake = true,
                TermEvent::Exit | TermEvent::ChildExit(_) => {
                    self.exited = true;
                    wake = true;
                }
                other => log::trace!("unhandled terminal event: {other:?}"),
            }
        }
        wake
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_palette_first_8_match_normal() {
        let p = ConfigColors::default();
        assert_eq!(indexed_from_palette(0, &p), p.normal.black);
        assert_eq!(indexed_from_palette(1, &p), p.normal.red);
        assert_eq!(indexed_from_palette(7, &p), p.normal.white);
    }

    #[test]
    fn indexed_palette_bright_block_match_bright() {
        let p = ConfigColors::default();
        assert_eq!(indexed_from_palette(8, &p), p.bright.black);
        assert_eq!(indexed_from_palette(15, &p), p.bright.white);
    }

    #[test]
    fn indexed_palette_color_cube_endpoints() {
        let p = ConfigColors::default();
        // idx 16 is the (0,0,0) corner of the 6x6x6 cube.
        assert_eq!(indexed_from_palette(16, &p), [0, 0, 0]);
        // idx 231 is (5,5,5): r=g=b = 55 + 40*5 = 255.
        assert_eq!(indexed_from_palette(231, &p), [255, 255, 255]);
    }

    #[test]
    fn indexed_palette_greyscale_endpoints() {
        let p = ConfigColors::default();
        // idx 232: 8 + 10*0 = 8.
        assert_eq!(indexed_from_palette(232, &p), [8, 8, 8]);
        // idx 255: 8 + 10*23 = 238.
        assert_eq!(indexed_from_palette(255, &p), [238, 238, 238]);
    }

    #[test]
    fn dim_reduces_intensity() {
        assert_eq!(dim([100, 200, 0]), [66, 132, 0]);
        assert_eq!(dim([0, 0, 0]), [0, 0, 0]);
    }
}

