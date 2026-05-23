use std::sync::Arc;

use alacritty_terminal::grid::Scroll;
use arboard::Clipboard;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

/// Sent by the alacritty PTY thread to wake the winit event loop when the
/// terminal has produced new content.
#[derive(Debug, Clone, Copy)]
pub struct WakeEvent;

mod input;

mod config;
mod gfx;
mod quad;
mod terminal;

use config::Config;
use gfx::Gfx;
use terminal::TerminalSession;

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
        }
    }

    fn spawn_tab(&mut self) {
        let Some(gfx) = self.gfx.as_ref() else { return };
        let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
        match TerminalSession::spawn(
            cols,
            lines,
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
        let Some(session) = self.tabs.get(self.active_tab) else { return };
        let Some(text) = session.selection_text() else { return };
        let Some(cb) = self.clipboard.as_mut() else { return };
        if let Err(e) = cb.set_text(text) {
            log::warn!("clipboard set_text: {e}");
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
        let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
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
            .with_title("alacritty-tabs")
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
        let session = TerminalSession::spawn(
            cols,
            lines,
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
        let Some(gfx) = self.gfx.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                gfx.resize(size);
                let (cols, lines) = gfx.grid_for_window(TAB_BAR_HEIGHT);
                let scale = window.scale_factor() as f32;
                let (cw, ch) = gfx.cell_dims_logical();
                let cell_w_px = (cw * scale).round().max(1.0) as u16;
                let cell_h_px = (ch * scale).round().max(1.0) as u16;
                for tab in &mut self.tabs {
                    tab.resize(cols, lines, cell_w_px, cell_h_px);
                }
                window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                let active_tab = self.active_tab;
                let term = self.tabs.get(active_tab);
                if let Err(e) = gfx.render(term, &self.tabs, active_tab, TAB_BAR_HEIGHT) {
                    log::error!("render error: {e}");
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.mods = mods.state();
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        text,
                        repeat: _,
                        ..
                    },
                ..
            } => {
                // Cmd-prefixed shortcuts are app-level; consume them here.
                if self.mods.super_key() {
                    if self.handle_app_shortcut(&logical_key, event_loop) {
                        window.request_redraw();
                        return;
                    }
                    return;
                }
                // Shift+PageUp / Shift+PageDown scroll the viewport over
                // scrollback before the keystroke is sent to the PTY.
                if self.mods.shift_key() {
                    if let Key::Named(named) = &logical_key {
                        let scroll = match named {
                            NamedKey::PageUp => Some(Scroll::PageUp),
                            NamedKey::PageDown => Some(Scroll::PageDown),
                            NamedKey::Home => Some(Scroll::Top),
                            NamedKey::End => Some(Scroll::Bottom),
                            _ => None,
                        };
                        if let Some(s) = scroll {
                            if let Some(session) = self.tabs.get(self.active_tab) {
                                session.scroll(s);
                                window.request_redraw();
                                return;
                            }
                        }
                    }
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
                if y_px <= tab_bar_h_px {
                    if let Some(idx) = gfx.tab_at_x(x_px as f32) {
                        self.select_tab(idx);
                        window.request_redraw();
                    }
                    return;
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

impl App {
    /// Returns true if the shortcut was handled (caller should swallow it).
    fn handle_app_shortcut(&mut self, key: &Key, event_loop: &ActiveEventLoop) -> bool {
        match key {
            Key::Character(s) => match s.as_str() {
                "t" | "T" => {
                    self.spawn_tab();
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    true
                }
                "w" | "W" => {
                    self.close_active_tab(event_loop);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    true
                }
                "c" | "C" => {
                    self.copy_selection();
                    true
                }
                "v" | "V" => {
                    self.paste();
                    true
                }
                "1" => { self.select_tab(0); true }
                "2" => { self.select_tab(1); true }
                "3" => { self.select_tab(2); true }
                "4" => { self.select_tab(3); true }
                "5" => { self.select_tab(4); true }
                "6" => { self.select_tab(5); true }
                "7" => { self.select_tab(6); true }
                "8" => { self.select_tab(7); true }
                "9" => { self.select_tab(8); true }
                _ => false,
            },
            Key::Named(NamedKey::ArrowLeft) => {
                if self.active_tab > 0 {
                    self.active_tab -= 1;
                }
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                if self.active_tab + 1 < self.tabs.len() {
                    self.active_tab += 1;
                }
                true
            }
            _ => false,
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
