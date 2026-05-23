use std::sync::Arc;

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::search::RegexSearch;
use arboard::Clipboard;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{ModifiersState, PhysicalKey};
use winit::window::{CursorIcon, Window, WindowId};

/// Sent by the alacritty PTY thread to wake the winit event loop when the
/// terminal has produced new content.
#[derive(Debug, Clone, Copy)]
pub struct WakeEvent;

mod input;

mod binding;
mod config;
mod gfx;
mod quad;
mod terminal;

use binding::Action;
use config::Config;
use gfx::Gfx;
use terminal::{TerminalSession, UrlMatch};

const TAB_BAR_HEIGHT: f32 = 28.0;

struct App {
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
}

impl App {
    fn new(config: Config, proxy: EventLoopProxy<WakeEvent>) -> Self {
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
            scroll_accum: 0.0,
            clipboard: Clipboard::new()
                .map_err(|e| log::warn!("clipboard unavailable: {e}"))
                .ok(),
            url_regex: terminal::compile_url_regex(),
            hover_url: None,
            cursor_icon: CursorIcon::Default,
        }
    }

    fn spawn_tab(&mut self) {
        let Some(gfx) = self.gfx.as_ref() else { return };
        let Some(window) = self.window.as_ref() else { return };
        let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
        let (cell_w_px, cell_h_px) = cell_dims_px(gfx, window);
        match TerminalSession::spawn(
            cols,
            lines,
            cell_w_px,
            cell_h_px,
            self.proxy.clone(),
            self.config.colors.clone(),
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
        let Some(session) = self.tabs.get(self.active_tab) else { return };
        let Some(text) = session.selection_text() else { return };
        if text.len() > MAX_COPY_BYTES {
            log::warn!(
                "selection is {} bytes; refusing to copy more than {} bytes to the clipboard",
                text.len(),
                MAX_COPY_BYTES
            );
            return;
        }
        let Some(cb) = self.clipboard.as_mut() else { return };
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

        let new_url = self.compute_hover_url();

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
        if self.cursor_icon != CursorIcon::Default {
            window.set_cursor(CursorIcon::Default);
            self.cursor_icon = CursorIcon::Default;
        }
        if had_url {
            window.request_redraw();
        }
    }

    fn compute_hover_url(&mut self) -> Option<UrlMatch> {
        let gfx = self.gfx.as_ref()?;
        let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
        let (line, col, _) = gfx.cell_at(
            self.cursor_pos.0 as f32,
            self.cursor_pos.1 as f32,
            TAB_BAR_HEIGHT,
            cols,
            lines,
        )?;
        let session = self.tabs.get(self.active_tab)?;
        if url_modifier_held(self.mods) {
            let regex = self.url_regex.as_mut()?;
            session.url_at(regex, line, col)
        } else {
            session.osc8_at(line, col)
        }
    }

    fn paste(&mut self) {
        let Some(session) = self.tabs.get(self.active_tab) else { return };
        let Some(cb) = self.clipboard.as_mut() else { return };
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
        // Normalize line endings: terminals expect CR for newline-as-Enter.
        // Also strip the bracketed-paste *end* marker from the payload — if an
        // attacker can get text containing \x1b[201~ onto the clipboard, the
        // receiving app would otherwise see paste-end mid-payload and treat
        // the rest as typed input.
        let normalized = text
            .replace("\r\n", "\r")
            .replace('\n', "\r")
            .replace("\x1b[201~", "");
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
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
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
        let (cols, lines) = gfx_ref(&self.gfx).grid_for_window(TAB_BAR_HEIGHT);
        let (cell_w_px, cell_h_px) = cell_dims_px(gfx_ref(&self.gfx), &window);
        let session = TerminalSession::spawn(
            cols,
            lines,
            cell_w_px,
            cell_h_px,
            self.proxy.clone(),
            self.config.colors.clone(),
        )
        .expect("spawn terminal");
        self.tabs.push(session);
        self.active_tab = 0;
        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else { return };
        if self.gfx.is_none() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                let gfx = self.gfx.as_mut().unwrap();
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
                let gfx = self.gfx.as_mut().unwrap();
                if let Err(e) =
                    gfx.render(term, &self.tabs, active_tab, TAB_BAR_HEIGHT, hover_url)
                {
                    log::error!("render error: {e}");
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.mods = mods.state();
                self.refresh_hover_url(&window);
            }
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
                let Some(session) = self.tabs.get(self.active_tab) else { return };
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
                if self.selecting {
                    if let Some(session) = self.tabs.get(self.active_tab) {
                        let gfx = self.gfx.as_ref().unwrap();
                        let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
                        if let Some((line, col, right)) = gfx.cell_at(
                            position.x as f32,
                            position.y as f32,
                            TAB_BAR_HEIGHT,
                            cols,
                            lines,
                        ) {
                            session.selection_update(line, col, right);
                            window.request_redraw();
                        }
                    }
                } else {
                    self.refresh_hover_url(&window);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let scale = window.scale_factor();
                let y_px = self.cursor_pos.1;
                let tab_bar_h_px = TAB_BAR_HEIGHT as f64 * scale;
                let x_px = self.cursor_pos.0;
                let gfx = self.gfx.as_ref().unwrap();
                if y_px <= tab_bar_h_px {
                    if let Some(idx) = gfx.tab_at_x(x_px as f32) {
                        self.select_tab(idx);
                        window.request_redraw();
                    }
                    return;
                }
                // Modifier+click on a URL opens it instead of starting a
                // selection. Hover state is the source of truth — if we
                // underlined it, we'll open the same URL.
                if url_modifier_held(self.mods) {
                    if let Some(url) = self.hover_url.as_ref() {
                        open_url(&url.uri);
                        return;
                    }
                }
                // Click inside the grid begins (or replaces) a selection.
                if let Some(session) = self.tabs.get(self.active_tab) {
                    let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
                    if let Some((line, col, right)) = gfx.cell_at(
                        x_px as f32,
                        y_px as f32,
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
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.selecting = false;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(session) = self.tabs.get(self.active_tab) else { return };
                let scale = window.scale_factor() as f32;
                let gfx = self.gfx.as_ref().unwrap();
                let (_, cell_h) = gfx.cell_dims_logical();
                let cell_h_px = (cell_h * scale).max(1.0);
                let lines_delta = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / cell_h_px,
                };
                self.scroll_accum += lines_delta;
                let whole = self.scroll_accum.trunc() as i32;
                if whole != 0 {
                    self.scroll_accum -= whole as f32;
                    session.scroll(Scroll::Delta(whole));
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: WakeEvent) {
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

        if wake {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }
}

fn gfx_ref(opt: &Option<Gfx>) -> &Gfx {
    opt.as_ref().expect("gfx initialized")
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

impl App {
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
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn scroll_active(&self, scroll: Scroll) {
        if let Some(session) = self.tabs.get(self.active_tab) {
            session.scroll(scroll);
        }
    }

    fn adjust_font_size(&mut self, delta: f32) {
        let new_size = (self.config.font_size + delta).clamp(6.0, 72.0);
        self.set_font_size(new_size);
    }

    fn reset_font_size(&mut self) {
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
        let Some(window) = self.window.as_ref() else { return };
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

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let config = config::load();

    // Convenience for headless test runs: set ATABS_EXIT_AFTER_MS=5000 to make
    // the process exit cleanly after N ms.
    if let Ok(ms) = std::env::var("ATABS_EXIT_AFTER_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                log::info!("ATABS_EXIT_AFTER_MS elapsed; exiting");
                std::process::exit(0);
            });
        }
    }

    let event_loop = EventLoop::<WakeEvent>::with_user_event()
        .build()
        .expect("event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = App::new(config, proxy);
    event_loop.run_app(&mut app).expect("run app");
}
