use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyLoop, Msg, Notifier};

use winit::event_loop::EventLoopProxy;

use crate::config::Colors as ConfigColors;
use crate::WakeEvent;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::term::{point_to_viewport, viewport_to_point, TermMode};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use alacritty_terminal::Term;
use crossbeam_channel::{unbounded, Receiver, Sender};

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
    /// True for cells with SGR underline *or* an OSC 8 hyperlink — the latter
    /// are conventionally rendered underlined so the user can see they're
    /// clickable without holding a modifier.
    pub underline: bool,
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

/// One inclusive horizontal span of a URL in viewport coordinates. URLs that
/// wrap across rows produce multiple spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UrlSpan {
    pub line: usize,
    pub start_col: usize,
    pub end_col: usize,
}

#[derive(Clone, Debug)]
pub struct UrlMatch {
    pub uri: String,
    pub spans: Vec<UrlSpan>,
}

/// Application's mouse-reporting preference, sampled from TermMode at the
/// moment of a winit mouse event. The input layer decides whether to
/// forward to the PTY or handle locally (selection / scroll) based on this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseMode {
    pub reporting: MouseReporting,
    /// SGR encoding (DECSET 1006). When false, legacy X10 encoding applies.
    /// We only emit SGR — the legacy encoding can't represent cells past
    /// column 223 and is effectively obsolete.
    pub sgr: bool,
    /// DECSET 1007 — when true and no reporting mode is active, the app
    /// wants wheel scrolls translated into arrow keys (tmux/less idiom).
    pub alternate_scroll: bool,
    pub alt_screen: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseReporting {
    None,
    /// Press + release only (DECSET 1000).
    Click,
    /// Press + release + motion while a button is held (DECSET 1002).
    Drag,
    /// Press + release + all motion (DECSET 1003).
    AnyMotion,
}

#[cfg(target_os = "macos")]
fn default_shell() -> &'static str {
    "/bin/zsh"
}
#[cfg(not(target_os = "macos"))]
fn default_shell() -> &'static str {
    // /bin/sh is mandated by POSIX and present on every Linux distro and BSD.
    // We avoid hardcoding bash/zsh because not every minimal install has them.
    "/bin/sh"
}

/// Render a path with `$HOME` collapsed to `~`, falling back to the
/// lossy display form when the path isn't UTF-8.
fn abbreviate_home(path: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// URL regex borrowed from alacritty's hint mode defaults.
const URL_REGEX_PATTERN: &str =
    "(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file:|git://|ssh:|ftp://)\
     [^\u{0000}-\u{001F}\u{007F}-\u{009F}<>\"\\s{-}\\^⟨⟩`\\\\]+";

/// Bound the regex sweep across linewraps. Without a cap, a URL stretched
/// across the whole scrollback would force us to scan it all.
const URL_MAX_SEARCH_LINES: i32 = 100;

/// Compile the URL regex used by `url_at`. Returns `None` only if the
/// hardcoded pattern fails to build, which is a programmer error.
pub fn compile_url_regex() -> Option<RegexSearch> {
    match RegexSearch::new(URL_REGEX_PATTERN) {
        Ok(r) => Some(r),
        Err(e) => {
            log::warn!("failed to compile URL regex: {e}");
            None
        }
    }
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
        let pal = if bright {
            &palette.bright
        } else {
            &palette.normal
        };
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

/// Walk left and right from `point` on the same row, collecting cells that
/// share the OSC 8 hyperlink id. Returns a single-row span; OSC 8 links that
/// wrap to a new line aren't extended onto it (rare in practice and not worth
/// the complexity).
fn osc8_spans<T>(
    term: &alacritty_terminal::Term<T>,
    point: Point,
    id: &str,
    vp_line: usize,
    cols: usize,
) -> Vec<UrlSpan> {
    let row = &term.grid()[point.line];
    let mut start_col = point.column.0;
    while start_col > 0 {
        match row[Column(start_col - 1)].hyperlink() {
            Some(h) if h.id() == id => start_col -= 1,
            _ => break,
        }
    }
    let mut end_col = point.column.0;
    while end_col + 1 < cols {
        match row[Column(end_col + 1)].hyperlink() {
            Some(h) if h.id() == id => end_col += 1,
            _ => break,
        }
    }
    vec![UrlSpan {
        line: vp_line,
        start_col,
        end_col,
    }]
}

/// Convert a regex `Match` (inclusive grid-line range) into per-row viewport
/// spans, clamped to the visible grid. Returns empty if the entire match is
/// off-screen.
fn match_to_spans(
    start: Point,
    end: Point,
    display_offset: usize,
    vp_lines: usize,
    vp_cols: usize,
) -> Vec<UrlSpan> {
    let start_line = start.line.0 + display_offset as i32;
    let end_line = end.line.0 + display_offset as i32;
    if end_line < 0 || start_line >= vp_lines as i32 || vp_cols == 0 {
        return Vec::new();
    }
    let max_col = vp_cols - 1;
    let lo = start_line.max(0);
    let hi = end_line.min(vp_lines as i32 - 1);
    let mut spans = Vec::with_capacity((hi - lo + 1) as usize);
    for line in lo..=hi {
        let s = if line == start_line {
            start.column.0.min(max_col)
        } else {
            0
        };
        let e = if line == end_line {
            end.column.0.min(max_col)
        } else {
            max_col
        };
        spans.push(UrlSpan {
            line: line as usize,
            start_col: s,
            end_col: e,
        });
    }
    spans
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
    /// When false, OSC 0/1/2 title-change requests from the running program
    /// are ignored — the tab keeps its initial title.
    dynamic_title: bool,
    /// PID of the shell process spawned for this tab. Used to resolve the
    /// shell's current working directory when the user opens a new tab so
    /// it inherits the cd'd-to location.
    shell_pid: Option<u32>,
    /// A dup of the pty master fd, kept so we can call tcgetpgrp() to find
    /// the tty's foreground process for the tab title. The pty's own Drop
    /// sends SIGHUP explicitly (it doesn't rely on the master fd closing),
    /// so holding this extra fd doesn't change shutdown behaviour.
    #[cfg(unix)]
    tty_fd: Option<std::os::fd::OwnedFd>,
    /// Memoised result of [`tab_label`](Self::tab_label) with the instant it
    /// was computed. The label resolution polls the foreground process, which
    /// is heavy on macOS (sysctl(KERN_PROCARGS2) copies the target's whole
    /// args+env blob), and tab_label() is called several times per frame.
    /// Caching it for a short window keeps fast-output repaints cheap.
    #[cfg(unix)]
    label_cache: std::cell::RefCell<Option<(std::time::Instant, String)>>,
}

impl TerminalSession {
    pub fn spawn(
        cols: u16,
        lines: u16,
        cell_width: u16,
        cell_height: u16,
        proxy: EventLoopProxy<WakeEvent>,
        palette: ConfigColors,
        working_directory: Option<std::path::PathBuf>,
        dynamic_title: bool,
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
        // sandbox blocks. Explicitly use $SHELL so we spawn the shell directly
        // without `login`. Fall back to a platform-appropriate shell that is
        // virtually always present.
        let shell_path = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_shell().to_string());
        pty_options.shell = Some(Shell::new(shell_path, Vec::new()));
        // GUI-spawned processes (Finder/Spotlight/.app, .desktop launchers)
        // inherit an empty TERM, leaving ncurses programs unable to initialize.
        // Set it here so the shell works regardless of how aterm itself was
        // launched.
        pty_options
            .env
            .insert("TERM".into(), "xterm-256color".into());
        pty_options
            .env
            .insert("COLORTERM".into(), "truecolor".into());
        // Caller-supplied working directory wins (used to inherit the cwd
        // of the active tab on Cmd+T). Fall back to $HOME when aterm's
        // own cwd looks like the launcher default (`/`).
        pty_options.working_directory = working_directory.filter(|p| p.is_dir());
        if pty_options.working_directory.is_none()
            && std::env::current_dir()
                .map(|p| p == std::path::Path::new("/"))
                .unwrap_or(false)
        {
            pty_options.working_directory = dirs::home_dir();
        }
        // window_id is opaque metadata used by alacritty's PTY layer.
        let pty = tty::new(&pty_options, window_size, 0)?;
        // Capture the child PID *before* the pty is moved into PtyLoop —
        // we need it to look up the shell's cwd later via /proc on Linux
        // or proc_pidinfo on macOS.
        let shell_pid = pty.child().id();
        // Dup the master fd while we still hold the pty, so we can poll the
        // tty's foreground process group later (tcgetpgrp) for the title.
        #[cfg(unix)]
        let tty_fd = {
            use std::os::fd::AsFd;
            pty.file().as_fd().try_clone_to_owned().ok()
        };

        let (events_tx, events_rx) = unbounded();
        let listener = ChannelListener {
            tx: events_tx,
            proxy,
        };

        let term_config = TermConfig::default();
        let term = Term::new(term_config, &term_size, listener.clone());
        let term = Arc::new(FairMutex::new(term));

        let pty_loop = PtyLoop::new(
            term.clone(),
            listener,
            pty,
            pty_options.drain_on_exit,
            false,
        )?;
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
            dynamic_title,
            shell_pid: Some(shell_pid),
            #[cfg(unix)]
            tty_fd,
            #[cfg(unix)]
            label_cache: std::cell::RefCell::new(None),
        })
    }

    /// Best-effort lookup of the shell's current working directory.
    /// Returns None if the platform isn't supported, the shell process
    /// has exited, or the OS denied the read. Used to seed a new tab's
    /// cwd from the active tab.
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        self.shell_pid.and_then(crate::cwd::cwd_of_pid)
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

    /// Snapshot of the application's mouse-reporting configuration. Used by
    /// the input layer to decide between selection/scrolling (default) and
    /// forwarding mouse events to the PTY (when the app has opted in).
    pub fn mouse_mode(&self) -> MouseMode {
        let mode = *self.term.lock().mode();
        let reporting = if mode.contains(TermMode::MOUSE_MOTION) {
            MouseReporting::AnyMotion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseReporting::Drag
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseReporting::Click
        } else {
            MouseReporting::None
        };
        MouseMode {
            reporting,
            sgr: mode.contains(TermMode::SGR_MOUSE),
            // ALTERNATE_SCROLL is set by tmux/less etc. to convert wheel
            // events into arrow keys when reporting isn't active. Caller
            // checks this when no real reporting mode is in effect.
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Title to display in the tab strip / OS window, memoised for a short
    /// window. [`compute_tab_label`](Self::compute_tab_label) polls the tty's
    /// foreground process, which is heavy on macOS, and we call this several
    /// times per frame (twice per tab in the layout pass, once for the OS
    /// window title). The foreground job and its cwd change far slower than we
    /// repaint, so serving a label that's up to `TTL` stale is invisible while
    /// it keeps fast-output redraws off the syscall path.
    pub fn tab_label(&self) -> String {
        #[cfg(unix)]
        {
            const TTL: std::time::Duration = std::time::Duration::from_millis(250);
            if let Some((at, label)) = self.label_cache.borrow().as_ref() {
                if at.elapsed() < TTL {
                    return label.clone();
                }
            }
            let label = self.compute_tab_label();
            *self.label_cache.borrow_mut() = Some((std::time::Instant::now(), label.clone()));
            label
        }
        #[cfg(not(unix))]
        self.compute_tab_label()
    }

    /// Resolve the tab label from scratch. When a child program (htop, vim,
    /// claude, …) is running in the foreground we show its name and cwd as
    /// `name (cwd)`, since the shell stops updating its own OSC title while a
    /// foreground job runs. When the shell itself is in the foreground we
    /// return its bare title — the prompt already shows the cwd, so there's
    /// nothing useful to add.
    fn compute_tab_label(&self) -> String {
        let base = self.title();
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let Some(tty_fd) = self.tty_fd.as_ref() else {
                return base.to_string();
            };
            let Some(fg_pid) = crate::cwd::foreground_pid(tty_fd.as_raw_fd()) else {
                return base.to_string();
            };
            // tcgetpgrp returns the shell's pgid while it's at the prompt;
            // that equals the shell pid for our interactive shell, so skip
            // the suffix in that case — the prompt already shows the cwd.
            if self.shell_pid == Some(fg_pid) {
                return base.to_string();
            }
            // Prefer the name as invoked (argv[0]) over the kernel's comm: a
            // tool installed as a version-named binary behind a symlink
            // (…/versions/2.1.150 with a `claude` symlink) reports "2.1.150"
            // from comm but "claude" from argv[0] — the latter is what the
            // user typed and recognises. Fall back to comm when argv is
            // unreadable (e.g. a setuid program owned by another user).
            let name =
                crate::cwd::invoked_name(fg_pid).or_else(|| crate::cwd::process_name(fg_pid));
            // ssh is the one program whose own title we keep verbatim: its
            // *local* cwd (~ or wherever you launched it) says nothing useful,
            // while the remote shell's title — forwarded through ssh as OSC
            // 0/2 — names the host/path you actually care about. Every other
            // program gets `name (cwd)`; we can't honour a program's own title
            // by attribution because the shell's preexec title (zsh sets one
            // per command) races the tcsetpgrp handoff and gets misattributed
            // to the job, which is why this used to suppress the cwd for *all*
            // commands.
            if name.as_deref() == Some("ssh") {
                return base.to_string();
            }
            // The foreground program's own cwd can be unreadable even though it
            // exists — on macOS proc_pidinfo denies PROC_PIDVNODEPATHINFO for a
            // process whose euid differs from ours, which is the case for
            // setuid-root tools like /usr/bin/top. Fall back to the shell's
            // cwd: the program inherited it on launch and almost never chdirs
            // away, and the shell runs as us so its cwd is always readable.
            let cwd = crate::cwd::cwd_of_pid(fg_pid)
                .or_else(|| self.shell_pid.and_then(crate::cwd::cwd_of_pid))
                .map(|p| abbreviate_home(&p));
            match (name, cwd) {
                (Some(name), Some(cwd)) => format!("{name} ({cwd})"),
                (Some(name), None) => name,
                (None, Some(cwd)) => format!("{base} ({cwd})"),
                (None, None) => base.to_string(),
            }
        }
        #[cfg(not(unix))]
        base.to_string()
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
        let point = viewport_to_point(display_offset, Point::new(vp_line, Column(vp_col)));
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
        let point = viewport_to_point(display_offset, Point::new(vp_line, Column(vp_col)));
        let side = if right_half { Side::Right } else { Side::Left };
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    /// Resolve an OSC 8 hyperlink at the given viewport cell, if any. Pure
    /// cell-attribute lookup — does not run the URL regex. Used for hover
    /// preview without a modifier, since OSC 8 link text hides its URI.
    pub fn osc8_at(&self, vp_line: usize, vp_col: usize) -> Option<UrlMatch> {
        let term = self.term.lock();
        let lines = term.screen_lines();
        let cols = term.columns();
        if cols == 0 || lines == 0 || vp_line >= lines || vp_col >= cols {
            return None;
        }
        let display_offset = term.grid().display_offset();
        let point = viewport_to_point(display_offset, Point::new(vp_line, Column(vp_col)));
        let link = term.grid()[point].hyperlink()?;
        let uri = link.uri().to_string();
        let id = link.id().to_string();
        let spans = osc8_spans(&term, point, &id, vp_line, cols);
        Some(UrlMatch { uri, spans })
    }

    /// Resolve the URL (if any) at the given viewport cell. Tries OSC 8
    /// first; falls back to running the URL regex across the visible
    /// viewport (plus a bounded gutter for wrapped long URLs) and returns
    /// the match containing the point.
    pub fn url_at(
        &self,
        regex: &mut RegexSearch,
        vp_line: usize,
        vp_col: usize,
    ) -> Option<UrlMatch> {
        if let Some(m) = self.osc8_at(vp_line, vp_col) {
            return Some(m);
        }

        let term = self.term.lock();
        let lines = term.screen_lines();
        let cols = term.columns();
        if cols == 0 || lines == 0 || vp_line >= lines || vp_col >= cols {
            return None;
        }
        let display_offset = term.grid().display_offset();
        let point = viewport_to_point(display_offset, Point::new(vp_line, Column(vp_col)));

        let viewport_start = Line(-(display_offset as i32));
        let viewport_end = viewport_start + (lines as i32 - 1);
        let mut start = term.line_search_left(Point::new(viewport_start, Column(0)));
        let mut end =
            term.line_search_right(Point::new(viewport_end, Column(cols.saturating_sub(1))));
        start.line = start.line.max(viewport_start - URL_MAX_SEARCH_LINES);
        end.line = end.line.min(viewport_end + URL_MAX_SEARCH_LINES);

        let mat = RegexIter::new(start, end, Direction::Right, &term, regex)
            .skip_while(|rm| rm.end().line < viewport_start)
            .take_while(|rm| rm.start().line <= viewport_end)
            .find(|rm| rm.contains(&point))?;

        let uri = term.bounds_to_string(*mat.start(), *mat.end());
        let spans = match_to_spans(*mat.start(), *mat.end(), display_offset, lines, cols);
        Some(UrlMatch { uri, spans })
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|s| !s.is_empty())
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
        if let Err(e) = self.notifier.0.send(Msg::Resize(window_size)) {
            log::warn!("failed to notify PTY of resize: {e}");
        }
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
            .map(|_| (0..cols).map(|_| SnapCell::default()).collect())
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
            let underline =
                cell.hyperlink().is_some() || cell.flags.intersects(Flags::ALL_UNDERLINES);
            cells[vp.line][vp.column.0] = SnapCell {
                ch,
                fg,
                bg,
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline,
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

    /// Snapshot the visible viewport as plain text — one string per row, with
    /// trailing spaces trimmed. Wide-char spacer slots are dropped so two-col
    /// glyphs appear once. Used by the debug IPC for assertions in tests.
    pub fn snapshot_text(&self) -> Vec<String> {
        let snap = self.snapshot();
        snap.cells
            .iter()
            .map(|row| {
                let mut s: String = row
                    .iter()
                    .filter(|c| c.ch != '\0')
                    .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
                    .collect();
                let trimmed = s.trim_end().len();
                s.truncate(trimmed);
                s
            })
            .collect()
    }

    /// Drain any pending alacritty events. Returns whether a redraw is needed.
    pub fn pump_events(&mut self) -> bool {
        let mut wake = false;
        while let Ok(event) = self.events.try_recv() {
            match event {
                TermEvent::Title(t) => {
                    if self.dynamic_title {
                        self.title = t;
                    }
                }
                TermEvent::ResetTitle => {
                    if self.dynamic_title {
                        self.title = "shell".into();
                    }
                }
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

    #[test]
    fn url_regex_compiles() {
        // The pattern is hardcoded; a compile failure would silently disable
        // URL detection, so guard it with a build-time-ish check.
        assert!(compile_url_regex().is_some());
    }

    fn pt(line: i32, col: usize) -> Point {
        Point::new(Line(line), Column(col))
    }

    #[test]
    fn match_to_spans_single_line_in_viewport() {
        // Match on viewport row 2 (grid line -3 with display_offset 5),
        // columns 4..=10. Spans should be a single inclusive range.
        let spans = match_to_spans(pt(-3, 4), pt(-3, 10), 5, 10, 80);
        assert_eq!(
            spans,
            vec![UrlSpan {
                line: 2,
                start_col: 4,
                end_col: 10
            }]
        );
    }

    #[test]
    fn match_to_spans_multi_line_fills_middle_rows() {
        // Match from row 1 col 70 to row 3 col 5; middle row should span the
        // full width.
        let spans = match_to_spans(pt(-4, 70), pt(-2, 5), 5, 10, 80);
        assert_eq!(
            spans,
            vec![
                UrlSpan {
                    line: 1,
                    start_col: 70,
                    end_col: 79
                },
                UrlSpan {
                    line: 2,
                    start_col: 0,
                    end_col: 79
                },
                UrlSpan {
                    line: 3,
                    start_col: 0,
                    end_col: 5
                },
            ]
        );
    }

    #[test]
    fn match_to_spans_clips_off_screen_top_and_bottom() {
        // Match starts above the viewport (grid line -10, display_offset 5
        // → vp line -5) and ends inside it on row 1. Spans should start at
        // row 0.
        let spans = match_to_spans(pt(-10, 0), pt(-4, 3), 5, 10, 80);
        assert_eq!(spans.first().map(|s| s.line), Some(0));
        assert_eq!(spans.last().map(|s| (s.line, s.end_col)), Some((1, 3)));

        // Entirely below the viewport: no spans.
        let spans = match_to_spans(pt(20, 0), pt(20, 5), 5, 10, 80);
        assert!(spans.is_empty());
    }

    #[test]
    fn match_to_spans_handles_zero_width_grid() {
        let spans = match_to_spans(pt(0, 0), pt(0, 0), 0, 5, 0);
        assert!(spans.is_empty());
    }
}
