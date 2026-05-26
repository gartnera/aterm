//! The winit `ApplicationHandler` that drives aterm — window/event-loop
//! glue, input dispatch, and the per-tab state. `main.rs` builds one of
//! these and hands it to winit's run loop.

use std::sync::Arc;

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::search::RegexSearch;
use arboard::Clipboard;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{ModifiersState, PhysicalKey};
#[cfg(target_os = "macos")]
use winit::platform::macos::WindowAttributesExtMacOS;
use winit::window::{CursorIcon, Window, WindowId};

use crate::binding::{self, Action};
use crate::config::{self, Config};
#[cfg(unix)]
use crate::debug_ipc;
use crate::gfx::Gfx;
use crate::input;
use crate::terminal::{self, MouseReporting, TerminalSession, UrlMatch};
use crate::WakeEvent;

pub const TAB_BAR_HEIGHT: f32 = 28.0;

/// Logical-pixel inset reserved on the left edge of the tab bar so it doesn't
/// overlap macOS traffic-light buttons (close/minimize/zoom). On other
/// platforms the tab bar lives in its own strip below the window chrome, so
/// no inset is needed.
#[cfg(target_os = "macos")]
pub const TAB_BAR_LEFT_INSET: f32 = 78.0;
#[cfg(not(target_os = "macos"))]
pub const TAB_BAR_LEFT_INSET: f32 = 0.0;

pub struct App {
    config: Config,
    proxy: EventLoopProxy<WakeEvent>,
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    tabs: Vec<TerminalSession>,
    active_tab: usize,
    mods: ModifiersState,
    cursor_pos: (f64, f64),
    /// Whether a left-button drag is in progress over the grid.
    selecting: bool,
    /// Bitmask of mouse buttons currently pressed and being reported to the
    /// PTY (bit per `input::MouseButton`). Used to emit motion events with
    /// the correct button code while a drag is in progress.
    mouse_buttons_held: u8,
    /// Last cell (row, col) we reported to the PTY in mouse-motion mode,
    /// to suppress duplicate motion events that arrive faster than the
    /// grid resolution.
    last_mouse_cell: Option<(usize, usize)>,
    /// Accumulated wheel delta in lines (line-delta events arrive as i32;
    /// pixel-delta events get converted using the cell height).
    scroll_accum: f32,
    clipboard: Option<Clipboard>,
    /// Compiled URL regex shared by all sessions. `None` if the hardcoded
    /// pattern failed to compile (treated as no auto-URL support).
    url_regex: Option<RegexSearch>,
    /// URL currently under the cursor with the open-url modifier held. Cleared
    /// whenever the modifier is released or the cursor moves off the URL.
    hover_url: Option<UrlMatch>,
    /// Last cursor icon we set on the window, so we don't ask winit to repeat.
    cursor_icon: CursorIcon,
    /// Last cell (line, col) the cursor was over when we computed `hover_url`.
    /// Used to skip the URL regex scan on sub-cell mouse movement.
    last_hover_cell: Option<(usize, usize)>,
    /// Current window title — we only call set_title when the active tab's
    /// title actually changes, to avoid waking the window server every frame.
    window_title: String,
    /// Pending debug-IPC requests from the optional Unix socket. None unless
    /// `ATERM_DEBUG_SOCK` was set at startup.
    #[cfg(unix)]
    debug_rx: Option<crossbeam_channel::Receiver<debug_ipc::PendingRequest>>,
}

impl App {
    pub fn new(config: Config, proxy: EventLoopProxy<WakeEvent>) -> Self {
        #[cfg(unix)]
        let debug_rx = debug_ipc::start_if_enabled(proxy.clone());
        Self {
            config,
            proxy,
            window: None,
            gfx: None,
            tabs: Vec::new(),
            active_tab: 0,
            mods: ModifiersState::empty(),
            cursor_pos: (0.0, 0.0),
            selecting: false,
            mouse_buttons_held: 0,
            last_mouse_cell: None,
            scroll_accum: 0.0,
            clipboard: Clipboard::new()
                .map_err(|e| log::warn!("clipboard unavailable: {e}"))
                .ok(),
            url_regex: terminal::compile_url_regex(),
            hover_url: None,
            cursor_icon: CursorIcon::Default,
            last_hover_cell: None,
            window_title: String::new(),
            #[cfg(unix)]
            debug_rx,
        }
    }

    /// Update the OS window title to match the active tab, if it differs from
    /// what we last set. Called from any code path that may change which tab
    /// is active or what its title is.
    fn sync_window_title(&mut self, window: &Window) {
        let new_title = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.tab_label())
            .unwrap_or_else(|| "aterm".to_string());
        if new_title != self.window_title {
            window.set_title(&new_title);
            self.window_title = new_title;
        }
    }

    fn spawn_tab(&mut self) {
        let Some(gfx) = self.gfx.as_ref() else { return };
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
        let (cell_w_px, cell_h_px) = cell_dims_px(gfx, window);
        // Inherit the active tab's cwd so e.g. `cd src; Cmd+T` opens the
        // new tab in `src`. Falls back to None (i.e. let the shell start
        // wherever it normally would) if the lookup fails or there is no
        // active tab — Wayland/Windows etc.
        let cwd = self.tabs.get(self.active_tab).and_then(|t| t.cwd());
        match TerminalSession::spawn(
            cols,
            lines,
            cell_w_px,
            cell_h_px,
            self.proxy.clone(),
            self.config.colors.clone(),
            cwd,
            self.config.dynamic_title,
        ) {
            Ok(s) => {
                self.tabs.push(s);
                self.active_tab = self.tabs.len() - 1;
            }
            Err(e) => log::error!("spawn tab: {e}"),
        }
    }

    fn close_active_tab(&mut self, event_loop: &ActiveEventLoop) {
        if self.tabs.is_empty() {
            return;
        }
        let idx = self.active_tab.min(self.tabs.len() - 1);
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            event_loop.exit();
            return;
        }
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
    }

    fn select_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
        }
    }

    fn copy_selection(&mut self) {
        // Defensive cap on copied size. Anything larger than this is almost
        // certainly a mis-drag across the entire scrollback; the limit keeps
        // a runaway selection from pushing tens-of-MB onto the OS clipboard.
        const MAX_COPY_BYTES: usize = 16 * 1024 * 1024;
        let Some(session) = self.tabs.get(self.active_tab) else {
            return;
        };
        let Some(text) = session.selection_text() else {
            return;
        };
        if text.len() > MAX_COPY_BYTES {
            log::warn!(
                "selection is {} bytes; refusing to copy more than {} bytes to the clipboard",
                text.len(),
                MAX_COPY_BYTES
            );
            return;
        }
        let Some(cb) = self.clipboard.as_mut() else {
            return;
        };
        if let Err(e) = cb.set_text(text) {
            log::warn!("clipboard set_text: {e}");
        }
    }

    /// Recompute the URL under the cursor and update the cursor icon. Called
    /// on every cursor move and on modifier changes. OSC 8 hyperlinks always
    /// surface their URI on hover (the visible text masks the URL); plain
    /// URLs only surface under the open-url modifier. The pointer-cursor
    /// and click-to-open are still modifier-gated.
    fn refresh_hover_url(&mut self, window: &Window) {
        let prev_uri = self.hover_url.as_ref().map(|u| u.uri.clone());
        let prev_spans = self.hover_url.as_ref().map(|u| u.spans.clone());

        let (new_url, cell) = self.compute_hover_url();
        self.last_hover_cell = cell;

        let icon = if new_url.is_some() && url_modifier_held(self.mods) {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        };
        if icon != self.cursor_icon {
            window.set_cursor(icon);
            self.cursor_icon = icon;
        }

        let new_uri = new_url.as_ref().map(|u| u.uri.clone());
        let new_spans = new_url.as_ref().map(|u| u.spans.clone());
        self.hover_url = new_url;
        if prev_uri != new_uri || prev_spans != new_spans {
            window.request_redraw();
        }
    }

    /// Drop the hovered-URL state and reset the cursor icon. Used when focus
    /// or the pointer leaves the window so the open-url cursor doesn't get
    /// stranded across click-to-open.
    fn clear_hover_url(&mut self, window: &Window) {
        let had_url = self.hover_url.is_some();
        self.hover_url = None;
        self.last_hover_cell = None;
        if self.cursor_icon != CursorIcon::Default {
            window.set_cursor(CursorIcon::Default);
            self.cursor_icon = CursorIcon::Default;
        }
        if had_url {
            window.request_redraw();
        }
    }

    /// Compute the URL under the cursor, along with the (line, col) cell the
    /// cursor was over. The cell is returned so the caller can debounce: if it
    /// matches the last computed cell, the regex scan can be skipped.
    fn compute_hover_url(&mut self) -> (Option<UrlMatch>, Option<(usize, usize)>) {
        let Some(gfx) = self.gfx.as_ref() else {
            return (None, None);
        };
        let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
        let Some((line, col, _)) = gfx.cell_at(
            self.cursor_pos.0 as f32,
            self.cursor_pos.1 as f32,
            TAB_BAR_HEIGHT,
            cols,
            lines,
        ) else {
            return (None, None);
        };
        let Some(session) = self.tabs.get(self.active_tab) else {
            return (None, Some((line, col)));
        };
        let url = if url_modifier_held(self.mods) {
            self.url_regex
                .as_mut()
                .and_then(|regex| session.url_at(regex, line, col))
        } else {
            session.osc8_at(line, col)
        };
        (url, Some((line, col)))
    }

    fn paste(&mut self) {
        let Some(session) = self.tabs.get(self.active_tab) else {
            return;
        };
        let Some(cb) = self.clipboard.as_mut() else {
            return;
        };
        let text = match cb.get_text() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("clipboard get_text: {e}");
                return;
            }
        };
        if text.is_empty() {
            return;
        }
        // CRLF → CR (terminals interpret CR as Enter) and strip embedded
        // bracketed-paste end markers (\x1b[201~) so an attacker who can
        // stage text on the clipboard can't break out of paste mode and
        // have the rest of the payload re-interpreted as typed input.
        let normalized = input::normalize_paste(&text);
        session.scroll(alacritty_terminal::grid::Scroll::Bottom);
        if session.bracketed_paste() {
            session.send_input(b"\x1b[200~".to_vec());
            session.send_input(normalized.into_bytes());
            session.send_input(b"\x1b[201~".to_vec());
        } else {
            session.send_input(normalized.into_bytes());
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

impl ApplicationHandler<WakeEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("aterm")
            .with_inner_size(winit::dpi::LogicalSize::new(900.0, 600.0));
        // On macOS, extend the content view under a transparent, title-hidden
        // title bar so aterm's own tab strip occupies the title-bar area. The
        // traffic-light buttons remain interactive on top of the content.
        #[cfg(target_os = "macos")]
        let attrs = attrs
            .with_titlebar_transparent(true)
            .with_title_hidden(true)
            .with_fullsize_content_view(true);
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        // Enable IME events so dead keys and CJK composition flow through
        // WindowEvent::Ime. Without this, pressing e.g. Option+e on a US
        // layout (acute accent dead key) produces no character at all, and
        // CJK input methods can't compose. With it, the platform IME
        // produces Ime::Commit(s) once composition finishes; we forward the
        // committed bytes to the PTY just like typed input.
        window.set_ime_allowed(true);
        let line_height = (self.config.font_size * 1.25).round();
        let gfx = pollster::block_on(Gfx::new(
            window.clone(),
            self.config.font_size,
            line_height,
            self.config.font_family.clone(),
            self.config.colors.clone(),
        ));
        self.window = Some(window.clone());
        self.gfx = Some(gfx);

        // Spawn an initial terminal tab sized to the current window.
        let gfx = self.gfx.as_ref().expect("gfx just initialized");
        let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
        let (cell_w_px, cell_h_px) = cell_dims_px(gfx, &window);
        match TerminalSession::spawn(
            cols,
            lines,
            cell_w_px,
            cell_h_px,
            self.proxy.clone(),
            self.config.colors.clone(),
            None,
            self.config.dynamic_title,
        ) {
            Ok(s) => {
                self.tabs.push(s);
                self.active_tab = 0;
            }
            Err(e) => {
                log::error!("failed to spawn initial terminal: {e}");
                event_loop.exit();
                return;
            }
        }
        self.sync_window_title(&window);
        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if self.gfx.is_none() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                let Some(gfx) = self.gfx.as_mut() else { return };
                gfx.resize(size);
                let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
                let (cell_w_px, cell_h_px) = cell_dims_px(gfx, &window);
                for tab in &mut self.tabs {
                    tab.resize(cols, lines, cell_w_px, cell_h_px);
                }
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let active_tab = self.active_tab;
                let term = self.tabs.get(active_tab);
                let hover_url = self.hover_url.as_ref();
                let Some(gfx) = self.gfx.as_mut() else { return };
                if let Err(e) = gfx.render(
                    term,
                    &self.tabs,
                    active_tab,
                    TAB_BAR_HEIGHT,
                    TAB_BAR_LEFT_INSET,
                    hover_url,
                ) {
                    log::error!("render error: {e}");
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.mods = mods.state();
                self.refresh_hover_url(&window);
            }
            // IME composition output. Preedit/Enabled/Disabled don't write
            // to the PTY — the platform IME handles its own preview popup,
            // and we only commit bytes once composition is complete. This
            // covers dead keys (Option+e then e → "é" on macOS) and CJK
            // input as a single uniform path.
            WindowEvent::Ime(ime) => match ime {
                Ime::Commit(text) => {
                    if text.is_empty() {
                        return;
                    }
                    let Some(session) = self.tabs.get(self.active_tab) else {
                        return;
                    };
                    session.scroll(Scroll::Bottom);
                    session.clear_selection();
                    session.send_input(text.into_bytes());
                    window.request_redraw();
                }
                Ime::Enabled | Ime::Disabled | Ime::Preedit(..) => {}
            },
            // Focus loss / cursor leaving the window strands the modifier
            // state — we won't see the key release that happens in another
            // app's context. Reset so the pointer-cursor and underline don't
            // stay stuck across the click-to-open round trip.
            WindowEvent::Focused(false) | WindowEvent::CursorLeft { .. } => {
                self.mods = ModifiersState::empty();
                self.clear_hover_url(&window);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        physical_key,
                        logical_key,
                        text,
                        repeat: _,
                        ..
                    },
                ..
            } => {
                // Check app-level keybindings first, against the physical
                // key, so e.g. Option+1 on macOS triggers SelectTab1
                // regardless of the layout-derived character (which would be
                // "¡" on a US layout with Alt held).
                let binding_action = if let PhysicalKey::Code(code) = physical_key {
                    binding::find(&self.config.bindings, code, self.mods).map(|b| b.action)
                } else {
                    None
                };
                match binding_action {
                    // Explicit pass-through: user mapped this key to
                    // ReceiveChar to disable a default. Fall through to the
                    // PTY input encoder.
                    Some(Action::ReceiveChar) => {}
                    Some(action) => {
                        self.run_action(action, event_loop);
                        return;
                    }
                    None if self.mods.super_key() => {
                        // Reserve Cmd-prefixed keys for the OS / app — don't
                        // leak unbound combos (e.g., Cmd+Q) to the shell.
                        return;
                    }
                    None => {}
                }
                let Some(session) = self.tabs.get(self.active_tab) else {
                    return;
                };
                let term_mode = input::TermKeyMode {
                    app_cursor: session.app_cursor_mode(),
                };
                if let Some(bytes) =
                    input::encode_key(&logical_key, text.as_deref(), self.mods, term_mode)
                {
                    // Typing snaps the viewport back to live output and
                    // clears any drag-selection (matches alacritty/iTerm).
                    session.scroll(Scroll::Bottom);
                    session.clear_selection();
                    session.send_input(bytes);
                    window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
                self.handle_cursor_moved(position.x, position.y, &window);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_button(state, button, &window);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta, &window);
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: WakeEvent) {
        // Handle any debug-IPC requests first so tests see fresh state.
        #[cfg(unix)]
        self.drain_debug_requests(event_loop);

        // The PTY thread woke us; drain its events and ask for a redraw if
        // any tab actually produced new content.
        let mut wake = false;
        for tab in &mut self.tabs {
            if tab.pump_events() {
                wake = true;
            }
        }

        // Reap tabs whose shell has exited. If that empties the tab list,
        // close the app.
        let pre_len = self.tabs.len();
        let active_was_exited = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.is_exited())
            .unwrap_or(false);
        self.tabs.retain(|t| !t.is_exited());
        if self.tabs.len() != pre_len {
            wake = true;
        }
        if self.tabs.is_empty() {
            event_loop.exit();
            return;
        }
        if active_was_exited || self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }

        if let Some(w) = self.window.clone() {
            // Mirror the active tab's title (set via OSC 0/2) to the OS
            // window title so it appears in window-list switchers.
            self.sync_window_title(&w);
            if wake {
                w.request_redraw();
            }
        }
    }
}

/// Modifier that turns the cursor into a link-opener. Cmd on macOS, Ctrl
/// elsewhere — matches the convention in iTerm2 and alacritty's hint mode.
fn url_modifier_held(mods: ModifiersState) -> bool {
    #[cfg(target_os = "macos")]
    {
        mods.super_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        mods.control_key()
    }
}

/// Hand the URL off to the OS opener. We deliberately don't go through a
/// shell — `Command::arg` passes the URI as a single argv slot, so URLs
/// containing shell metacharacters can't escape the opener invocation.
fn open_url(uri: &str) {
    // Reject control characters defensively; the OS opener should reject
    // them too, but skipping the spawn avoids surprising error noise.
    if uri.chars().any(|c| c.is_control()) {
        log::warn!("refusing to open URL with control characters");
        return;
    }
    let result = spawn_url_opener(uri);
    if let Err(e) = result {
        log::warn!("failed to launch URL opener: {e}");
    }
}

#[cfg(target_os = "macos")]
fn spawn_url_opener(uri: &str) -> std::io::Result<std::process::Child> {
    std::process::Command::new("open").arg(uri).spawn()
}

#[cfg(target_os = "linux")]
fn spawn_url_opener(uri: &str) -> std::io::Result<std::process::Child> {
    std::process::Command::new("xdg-open").arg(uri).spawn()
}

#[cfg(target_os = "windows")]
fn spawn_url_opener(uri: &str) -> std::io::Result<std::process::Child> {
    // `start` takes a window-title argument before the URL.
    std::process::Command::new("cmd")
        .args(["/c", "start", "", uri])
        .spawn()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn spawn_url_opener(_uri: &str) -> std::io::Result<std::process::Child> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no URL opener configured for this platform",
    ))
}

fn cell_dims_px(gfx: &Gfx, window: &Window) -> (u16, u16) {
    let scale = window.scale_factor() as f32;
    let (cw, ch) = gfx.cell_dims_logical();
    (
        (cw * scale).round().max(1.0) as u16,
        (ch * scale).round().max(1.0) as u16,
    )
}

/// Translate a winit `MouseButton` to our reporting button, returning None
/// for buttons xterm doesn't encode (Back/Forward/extra).
fn mouse_button_for_report(button: MouseButton) -> Option<input::MouseButton> {
    match button {
        MouseButton::Left => Some(input::MouseButton::Left),
        MouseButton::Middle => Some(input::MouseButton::Middle),
        MouseButton::Right => Some(input::MouseButton::Right),
        _ => None,
    }
}

impl App {
    /// Compute the (row, col) cell under the cursor, or None if the pointer
    /// is over the tab bar / padding. Mouse-mode reporting needs this in
    /// every event handler.
    fn cell_under_cursor(&self) -> Option<(usize, usize)> {
        let gfx = self.gfx.as_ref()?;
        let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
        let (l, c, _) = gfx.cell_at(
            self.cursor_pos.0 as f32,
            self.cursor_pos.1 as f32,
            TAB_BAR_HEIGHT,
            cols,
            lines,
        )?;
        Some((l, c))
    }

    fn pointer_in_tab_bar(&self, window: &Window) -> bool {
        self.cursor_pos.1 <= TAB_BAR_HEIGHT as f64 * window.scale_factor()
    }

    fn handle_cursor_moved(&mut self, x: f64, y: f64, window: &Window) {
        // Mouse-reporting motion takes priority over local drag/hover when
        // an application has subscribed to drag or all-motion events.
        if self.maybe_report_mouse_motion(window) {
            return;
        }
        if self.selecting {
            let Some(gfx) = self.gfx.as_ref() else { return };
            let Some(session) = self.tabs.get(self.active_tab) else {
                return;
            };
            let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
            if let Some((line, col, right)) =
                gfx.cell_at(x as f32, y as f32, TAB_BAR_HEIGHT, cols, lines)
            {
                session.selection_update(line, col, right);
                window.request_redraw();
            }
        } else {
            // Sub-cell mouse movement is common (every winit cursor event);
            // skip the OSC 8 lookup / URL regex scan when the cursor is
            // still in the same grid cell as last time.
            let cell = self.gfx.as_ref().and_then(|gfx| {
                let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
                gfx.cell_at(x as f32, y as f32, TAB_BAR_HEIGHT, cols, lines)
                    .map(|(l, c, _)| (l, c))
            });
            if cell != self.last_hover_cell {
                self.refresh_hover_url(window);
            }
        }
    }

    /// If the active app has enabled drag-tracking (1002) or any-motion
    /// (1003) reporting and we're inside the grid, emit a motion event to
    /// the PTY and return true. Returns false otherwise so the caller falls
    /// through to local handling (selection drag, hover URL).
    fn maybe_report_mouse_motion(&mut self, _window: &Window) -> bool {
        // Shift always bypasses mouse reporting so the user can still drag
        // a selection over a full-screen app (xterm convention).
        if self.mods.shift_key() {
            return false;
        }
        let Some(session) = self.tabs.get(self.active_tab) else {
            return false;
        };
        let mode = session.mouse_mode();
        let wants_motion = match mode.reporting {
            MouseReporting::AnyMotion => true,
            MouseReporting::Drag => self.mouse_buttons_held != 0,
            MouseReporting::Click | MouseReporting::None => false,
        };
        if !wants_motion {
            return false;
        }
        let Some((row, col)) = self.cell_under_cursor() else {
            return false;
        };
        if Some((row, col)) == self.last_mouse_cell {
            return true;
        }
        self.last_mouse_cell = Some((row, col));
        // Pick a representative held button (lowest bit) for the motion
        // event. If nothing is held (AnyMotion mode) report as Left+motion;
        // that's what xterm does — the receiver only cares about position.
        let button = if self.mouse_buttons_held & (1 << 0) != 0 {
            input::MouseButton::Left
        } else if self.mouse_buttons_held & (1 << 1) != 0 {
            input::MouseButton::Middle
        } else if self.mouse_buttons_held & (1 << 2) != 0 {
            input::MouseButton::Right
        } else {
            input::MouseButton::Left
        };
        if mode.sgr {
            let bytes =
                input::encode_mouse_sgr(button, input::MouseAction::Motion, col, row, self.mods);
            session.send_input(bytes);
        }
        true
    }

    fn handle_mouse_button(&mut self, state: ElementState, button: MouseButton, window: &Window) {
        // Tab-bar click — only left button picks tabs, and we never report
        // tab-bar interactions to the PTY.
        if state == ElementState::Pressed
            && button == MouseButton::Left
            && self.pointer_in_tab_bar(window)
        {
            let Some(gfx) = self.gfx.as_ref() else { return };
            if let Some(idx) = gfx.tab_at_x(self.cursor_pos.0 as f32) {
                self.select_tab(idx);
                self.sync_window_title(window);
                window.request_redraw();
                return;
            }
            // Empty area of the tab bar on macOS doubles as a title-bar
            // drag handle, matching how Safari/Terminal/Ghostty behave.
            #[cfg(target_os = "macos")]
            {
                let _ = window.drag_window();
            }
            return;
        }

        // Mouse-reporting path: forward to PTY if the app has subscribed
        // and the user isn't holding Shift to bypass. Modifier+click on a
        // hovered URL also bypasses reporting so click-to-open keeps
        // working inside tmux / vim / etc.
        let url_open_click =
            button == MouseButton::Left && url_modifier_held(self.mods) && self.hover_url.is_some();
        if !self.mods.shift_key() && !url_open_click {
            if let Some(report_btn) = mouse_button_for_report(button) {
                if self.try_report_mouse_button(state, report_btn) {
                    return;
                }
            }
        }

        // Local behavior: left-click selects / opens URL; left-release
        // ends a drag. Other buttons have no local meaning today.
        if button != MouseButton::Left {
            return;
        }
        match state {
            ElementState::Pressed => {
                // Modifier+click on a URL opens it instead of starting a
                // selection. Hover state is the source of truth — if we
                // underlined it, we'll open the same URL.
                if url_modifier_held(self.mods) {
                    if let Some(url) = self.hover_url.as_ref() {
                        open_url(&url.uri);
                        return;
                    }
                }
                let Some(gfx) = self.gfx.as_ref() else { return };
                let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
                if let Some(session) = self.tabs.get(self.active_tab) {
                    if let Some((line, col, right)) = gfx.cell_at(
                        self.cursor_pos.0 as f32,
                        self.cursor_pos.1 as f32,
                        TAB_BAR_HEIGHT,
                        cols,
                        lines,
                    ) {
                        session.selection_start(line, col, right);
                        self.selecting = true;
                        window.request_redraw();
                    }
                }
            }
            ElementState::Released => {
                self.selecting = false;
            }
        }
    }

    /// Send an SGR press/release event for a button if the active app is
    /// in any mouse-reporting mode. Returns true if the event was reported.
    /// Tracks `mouse_buttons_held` so subsequent motion events know which
    /// button to report.
    fn try_report_mouse_button(&mut self, state: ElementState, button: input::MouseButton) -> bool {
        let Some(session) = self.tabs.get(self.active_tab) else {
            return false;
        };
        let mode = session.mouse_mode();
        if matches!(mode.reporting, MouseReporting::None) {
            return false;
        }
        let Some((row, col)) = self.cell_under_cursor() else {
            return false;
        };
        let bit = match button {
            input::MouseButton::Left => 1 << 0,
            input::MouseButton::Middle => 1 << 1,
            input::MouseButton::Right => 1 << 2,
            // Wheel never goes through this path.
            _ => return false,
        };
        let (action, bytes) = match state {
            ElementState::Pressed => {
                self.mouse_buttons_held |= bit;
                self.last_mouse_cell = Some((row, col));
                (
                    input::MouseAction::Press,
                    input::encode_mouse_sgr(button, input::MouseAction::Press, col, row, self.mods),
                )
            }
            ElementState::Released => {
                self.mouse_buttons_held &= !bit;
                if self.mouse_buttons_held == 0 {
                    self.last_mouse_cell = None;
                }
                (
                    input::MouseAction::Release,
                    input::encode_mouse_sgr(
                        button,
                        input::MouseAction::Release,
                        col,
                        row,
                        self.mods,
                    ),
                )
            }
        };
        let _ = action;
        if mode.sgr {
            session.send_input(bytes);
            true
        } else {
            // Legacy X10 encoding isn't implemented — refuse to fall back
            // to it so we don't garble coordinates past col 223. The app
            // will retry with SGR if it cares (most modern apps request
            // 1006 alongside 1000/1002/1003).
            false
        }
    }

    fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta, window: &Window) {
        let Some(session) = self.tabs.get(self.active_tab) else {
            return;
        };
        let Some(gfx) = self.gfx.as_ref() else { return };
        let scale = window.scale_factor() as f32;
        let (_, cell_h) = gfx.cell_dims_logical();
        let cell_h_px = (cell_h * scale).max(1.0);
        let lines_delta = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(p) => p.y as f32 / cell_h_px,
        };
        self.scroll_accum += lines_delta;
        let whole = self.scroll_accum.trunc() as i32;
        if whole == 0 {
            return;
        }
        self.scroll_accum -= whole as f32;

        let mode = session.mouse_mode();
        let mouse_reporting_active = !matches!(mode.reporting, MouseReporting::None);

        // 1) App-driven mouse reporting — encode each notch as a wheel
        // button press. Shift held bypasses reporting so the user can
        // still scroll the local viewport over a full-screen app.
        if mouse_reporting_active && mode.sgr && !self.mods.shift_key() {
            let (row, col) = self.cell_under_cursor().unwrap_or((0, 0));
            let button = if whole > 0 {
                input::MouseButton::WheelUp
            } else {
                input::MouseButton::WheelDown
            };
            for _ in 0..whole.unsigned_abs() {
                let bytes =
                    input::encode_mouse_sgr(button, input::MouseAction::Press, col, row, self.mods);
                session.send_input(bytes);
            }
            window.request_redraw();
            return;
        }

        // 2) Alternate-scroll (DECSET 1007) on the alt screen — translate
        // wheel ticks to arrow-key presses so less/man/vim respond.
        if mode.alternate_scroll && mode.alt_screen && !self.mods.shift_key() {
            let arrow: &[u8] = if session.app_cursor_mode() {
                if whole > 0 {
                    b"\x1bOA"
                } else {
                    b"\x1bOB"
                }
            } else if whole > 0 {
                b"\x1b[A"
            } else {
                b"\x1b[B"
            };
            for _ in 0..whole.unsigned_abs() {
                session.send_input(arrow.to_vec());
            }
            window.request_redraw();
            return;
        }

        // 3) Default: scroll the local viewport.
        session.scroll(Scroll::Delta(whole));
        window.request_redraw();
    }

    fn run_action(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        match action {
            Action::CreateTab => self.spawn_tab(),
            Action::CloseTab => self.close_active_tab(event_loop),
            Action::SelectTab(n) => self.select_tab((n as usize).saturating_sub(1)),
            Action::PrevTab => {
                if self.active_tab > 0 {
                    self.active_tab -= 1;
                }
            }
            Action::NextTab => {
                if self.active_tab + 1 < self.tabs.len() {
                    self.active_tab += 1;
                }
            }
            Action::Copy => self.copy_selection(),
            Action::Paste => self.paste(),
            Action::ScrollLineUp => self.scroll_active(Scroll::Delta(1)),
            Action::ScrollLineDown => self.scroll_active(Scroll::Delta(-1)),
            Action::ScrollPageUp => self.scroll_active(Scroll::PageUp),
            Action::ScrollPageDown => self.scroll_active(Scroll::PageDown),
            Action::ScrollToTop => self.scroll_active(Scroll::Top),
            Action::ScrollToBottom => self.scroll_active(Scroll::Bottom),
            Action::IncreaseFontSize => self.adjust_font_size(1.0),
            Action::DecreaseFontSize => self.adjust_font_size(-1.0),
            Action::ResetFontSize => self.reset_font_size(),
            // Handled at the dispatch site (key falls through to PTY); if
            // we somehow get here, treat it as a no-op.
            Action::ReceiveChar => {}
        }
        if let Some(w) = self.window.clone() {
            self.sync_window_title(&w);
            w.request_redraw();
        }
    }

    fn scroll_active(&self, scroll: Scroll) {
        if let Some(session) = self.tabs.get(self.active_tab) {
            session.scroll(scroll);
        }
    }

    pub(crate) fn adjust_font_size(&mut self, delta: f32) {
        let new_size = (self.config.font_size + delta).clamp(6.0, 72.0);
        self.set_font_size(new_size);
    }

    pub(crate) fn reset_font_size(&mut self) {
        // Restore the size the user set in alacritty.toml (captured at load
        // time). If they didn't set one, fall back to the built-in default.
        let target = self
            .config
            .font_size_initial
            .unwrap_or_else(|| config::Config::default().font_size);
        self.set_font_size(target);
    }

    fn set_font_size(&mut self, size: f32) {
        if (self.config.font_size - size).abs() < f32::EPSILON {
            return;
        }
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(window) = self.window.as_ref() else {
            return;
        };
        self.config.font_size = size;
        let line_height = (size * 1.25).round();
        gfx.set_font_metrics(size, line_height);
        let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
        let (cell_w_px, cell_h_px) = cell_dims_px(gfx, window);
        for tab in &mut self.tabs {
            tab.resize(cols, lines, cell_w_px, cell_h_px);
        }
    }
}

#[cfg(unix)]
impl App {
    fn drain_debug_requests(&mut self, event_loop: &ActiveEventLoop) {
        let Some(rx) = self.debug_rx.as_ref() else {
            return;
        };
        // try_iter cuts off as soon as the channel is empty so we don't block.
        let pending: Vec<debug_ipc::PendingRequest> = rx.try_iter().collect();
        for req in pending {
            let resp = self.handle_debug(event_loop, req.request);
            let _ = req.reply.send(resp);
        }
    }

    fn handle_debug(
        &mut self,
        event_loop: &ActiveEventLoop,
        req: debug_ipc::Request,
    ) -> debug_ipc::Response {
        use debug_ipc::{Request, Response};
        match req {
            Request::SnapshotText => match self.tabs.get(self.active_tab) {
                Some(t) => {
                    let lines = t.snapshot_text();
                    Response::ok_data(serde_json::json!({ "lines": lines }))
                }
                None => Response::err("no active tab"),
            },
            Request::Tabs => {
                let tabs: Vec<_> = self
                    .tabs
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        serde_json::json!({
                            "index": i,
                            "title": t.tab_label(),
                            "active": i == self.active_tab,
                        })
                    })
                    .collect();
                Response::ok_data(serde_json::json!({ "tabs": tabs }))
            }
            Request::Title => Response::ok_data(serde_json::json!({
                "title": self.window_title,
            })),
            Request::TypeBytes { bytes } => match self.tabs.get(self.active_tab) {
                Some(t) => {
                    t.scroll(Scroll::Bottom);
                    t.send_input(bytes);
                    Response::ok_empty()
                }
                None => Response::err("no active tab"),
            },
            Request::CreateTab => {
                let before = self.tabs.len();
                self.spawn_tab();
                if let Some(w) = self.window.clone() {
                    self.sync_window_title(&w);
                    w.request_redraw();
                }
                Response::ok_data(serde_json::json!({
                    "created": self.tabs.len() > before,
                    "active": self.active_tab,
                }))
            }
            Request::CloseTab => {
                self.close_active_tab(event_loop);
                if let Some(w) = self.window.clone() {
                    self.sync_window_title(&w);
                    w.request_redraw();
                }
                Response::ok_data(serde_json::json!({
                    "tabs_remaining": self.tabs.len(),
                }))
            }
            Request::SelectTab { index } => {
                if index >= self.tabs.len() {
                    return Response::err(format!(
                        "tab index {index} out of range (have {})",
                        self.tabs.len()
                    ));
                }
                self.select_tab(index);
                if let Some(w) = self.window.clone() {
                    self.sync_window_title(&w);
                    w.request_redraw();
                }
                Response::ok_data(serde_json::json!({ "active": self.active_tab }))
            }
            Request::FontSize { delta } => {
                self.adjust_font_size(delta);
                Response::ok_data(serde_json::json!({
                    "font_size": self.config.font_size,
                }))
            }
            Request::FontSizeReset => {
                self.reset_font_size();
                Response::ok_data(serde_json::json!({
                    "font_size": self.config.font_size,
                }))
            }
            Request::HoverUrl { row, col, ctrl } => {
                let Some(session) = self.tabs.get(self.active_tab) else {
                    return Response::err("no active tab");
                };
                let url = if ctrl {
                    self.url_regex
                        .as_mut()
                        .and_then(|re| session.url_at(re, row, col))
                } else {
                    session.osc8_at(row, col)
                };
                match url {
                    Some(u) => Response::ok_data(serde_json::json!({ "uri": u.uri })),
                    None => Response::ok_data(serde_json::Value::Null),
                }
            }
        }
    }
}
