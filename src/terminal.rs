use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{point_to_viewport, TermMode};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use alacritty_terminal::Term;
use crossbeam_channel::{Receiver, Sender, unbounded};

#[derive(Clone)]
pub struct ChannelListener(Sender<TermEvent>);

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
}

enum Role {
    Fg,
    Bg,
}

fn rgb_to_arr(rgb: Rgb) -> [u8; 3] {
    [rgb.r, rgb.g, rgb.b]
}

fn default_fg() -> Rgb {
    Rgb { r: 0xd0, g: 0xd0, b: 0xd0 }
}

fn default_bg() -> Rgb {
    Rgb { r: 0x10, g: 0x10, b: 0x14 }
}

fn xterm_256(idx: u8) -> Rgb {
    // Standard xterm palette. Used both for direct Indexed lookups and as a
    // fallback for Named colors when the Term hasn't been told a palette
    // (which is the normal case — alacritty's `Term::colors` is all-None
    // until the application sets it via OSC sequences or we seed it).
    static BASIC: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00), (0xcd, 0x31, 0x31), (0x0d, 0xbc, 0x79), (0xe5, 0xe5, 0x10),
        (0x24, 0x72, 0xc8), (0xbc, 0x3f, 0xbc), (0x11, 0xa8, 0xcd), (0xe5, 0xe5, 0xe5),
        (0x66, 0x66, 0x66), (0xf1, 0x4c, 0x4c), (0x23, 0xd1, 0x8b), (0xf5, 0xf5, 0x43),
        (0x3b, 0x8e, 0xea), (0xd6, 0x70, 0xd6), (0x29, 0xb8, 0xdb), (0xe5, 0xe5, 0xe5),
    ];
    if idx < 16 {
        let (r, g, b) = BASIC[idx as usize];
        return Rgb { r, g, b };
    }
    if (16..=231).contains(&idx) {
        let n = idx - 16;
        let r = n / 36;
        let g = (n / 6) % 6;
        let b = n % 6;
        let scale = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
        return Rgb { r: scale(r), g: scale(g), b: scale(b) };
    }
    let gray = 8 + 10 * (idx - 232);
    Rgb { r: gray, g: gray, b: gray }
}

fn named_to_basic_idx(name: NamedColor) -> Option<u8> {
    Some(match name {
        NamedColor::Black => 0,
        NamedColor::Red => 1,
        NamedColor::Green => 2,
        NamedColor::Yellow => 3,
        NamedColor::Blue => 4,
        NamedColor::Magenta => 5,
        NamedColor::Cyan => 6,
        NamedColor::White => 7,
        NamedColor::BrightBlack => 8,
        NamedColor::BrightRed => 9,
        NamedColor::BrightGreen => 10,
        NamedColor::BrightYellow => 11,
        NamedColor::BrightBlue => 12,
        NamedColor::BrightMagenta => 13,
        NamedColor::BrightCyan => 14,
        NamedColor::BrightWhite => 15,
        _ => return None,
    })
}

fn resolve_color(
    color: AnsiColor,
    colors: &alacritty_terminal::term::color::Colors,
    role: Role,
) -> [u8; 3] {
    let rgb = match color {
        AnsiColor::Named(name) => colors[name].unwrap_or_else(|| {
            if let Some(idx) = named_to_basic_idx(name) {
                xterm_256(idx)
            } else {
                match role {
                    Role::Fg => default_fg(),
                    Role::Bg => default_bg(),
                }
            }
        }),
        AnsiColor::Spec(rgb) => rgb,
        AnsiColor::Indexed(idx) => colors[idx as usize].unwrap_or_else(|| xterm_256(idx)),
    };
    rgb_to_arr(rgb)
}

impl EventListener for ChannelListener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.0.send(event);
    }
}

pub struct TerminalSession {
    term: Arc<FairMutex<Term<ChannelListener>>>,
    notifier: Notifier,
    events: Receiver<TermEvent>,
    title: String,
    cols: u16,
    lines: u16,
}

impl TerminalSession {
    pub fn spawn(cols: u16, lines: u16) -> std::io::Result<Self> {
        // Approximate cell size; the renderer will refine this when it knows
        // the real per-cell pixel size and call `resize`.
        let cell_width = 8;
        let cell_height = 16;
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
        // window_id is opaque metadata used by alacritty's PTY layer.
        let pty = tty::new(&pty_options, window_size, 0)?;

        let (events_tx, events_rx) = unbounded();
        let listener = ChannelListener(events_tx);

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
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn send_input<B: Into<std::borrow::Cow<'static, [u8]>>>(&self, bytes: B) {
        self.notifier.notify(bytes);
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
        let reverse = content.mode.contains(TermMode::ALT_SCREEN); // unused; placeholder

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
            let mut fg = resolve_color(cell.fg, content.colors, Role::Fg);
            let mut bg = resolve_color(cell.bg, content.colors, Role::Bg);
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
        let _ = reverse;

        let cursor_vp = point_to_viewport(display_offset, cursor_point);
        GridSnapshot {
            cells,
            cursor_line: cursor_vp.map(|p| p.line).unwrap_or(0),
            cursor_col: cursor_vp.map(|p| p.column.0).unwrap_or(0),
            cursor_visible,
            fg: rgb_to_arr(content.colors[NamedColor::Foreground].unwrap_or(default_fg())),
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
                TermEvent::Exit => wake = true,
                _ => {}
            }
        }
        wake
    }
}

